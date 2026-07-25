package goclient

import (
	"context"
	"crypto/tls"
	"fmt"
	"net/http"
	"net/url"
	"strconv"
	"strings"
	"sync"
	"sync/atomic"
	"time"

	"github.com/quic-go/webtransport-go"
	"github.com/zR-JB/graphite-meter/go/internal/transport"
	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

// wtSession is a dialed WebTransport session and the dialer that owns it.
type wtSession struct {
	*webtransport.Session
	dialer *webtransport.Dialer
}

func (s *wtSession) close() {
	_ = s.CloseWithError(0, "")
	_ = s.dialer.Close()
}

// wtDial opens a session on origin's path. query carries every parameter: a
// stream has no URL of its own, so the CONNECT URL speaks for the session. An
// authentication grant rides the CONNECT's Authorization header, under the same
// origin pinning the HTTP transport applies.
func wtDial(ctx context.Context, cfg Config, origin, path string, query url.Values) (*wtSession, error) {
	u, err := httpEndpoint(origin, path)
	if err != nil {
		return nil, err
	}
	if len(query) > 0 {
		u += "?" + query.Encode()
	}
	var hdr http.Header
	if token := cfg.authToken(); token != "" {
		parsed, err := url.Parse(u)
		if err != nil || parsed.Scheme != "https" || !strings.EqualFold(parsed.Hostname(), pinnedHostname(cfg.AuthOrigin)) {
			return nil, fmt.Errorf("refusing to send authentication grant outside canonical HTTPS host")
		}
		hdr = http.Header{"Authorization": {"Bearer " + token}}
	}
	dialer := &webtransport.Dialer{
		TLSClientConfig: &tls.Config{InsecureSkipVerify: cfg.InsecureSkipTLSVerify}, //nolint:gosec
		QUICConfig:      transport.NewQUICConfig(),
	}
	_, sess, err := dialer.Dial(ctx, u, hdr)
	if err != nil {
		_ = dialer.Close()
		return nil, fmt.Errorf("webtransport dial %s: %w", u, err)
	}
	return &wtSession{Session: sess, dialer: dialer}, nil
}

// verifyLatencyWebTransport proves the datagram bus answers before a run
// commits to it, mirroring the WebSocket check.
func verifyLatencyWebTransport(ctx context.Context, cfg Config, target *wire.LatencyTarget) error {
	verifyCtx, cancel := context.WithTimeout(ctx, 3*time.Second)
	defer cancel()
	sess, err := wtDial(verifyCtx, cfg, target.Origin, target.Routes.WTPing, nil)
	if err != nil {
		return err
	}
	defer sess.close()
	// The hello and its reply are unacknowledged datagrams, so one lost packet
	// must not decide the transport for the whole run: retry within the window.
	var lastErr error
	for verifyCtx.Err() == nil {
		if err := sess.SendDatagram([]byte(wire.Encode(wire.Frame{Op: wire.OpHI, Proto: "wt"}))); err != nil {
			return fmt.Errorf("latency WebTransport hello failed: %w", err)
		}
		replyCtx, cancelReply := context.WithTimeout(verifyCtx, wtVerifyReplyTimeout)
		reply, err := sess.ReceiveDatagram(replyCtx)
		cancelReply()
		if err != nil {
			lastErr = err
			continue
		}
		if frame, err := wire.Decode(string(reply)); err == nil && frame.Op == wire.OpREADY {
			return nil
		}
	}
	if lastErr != nil {
		return fmt.Errorf("latency WebTransport readiness failed: %w", lastErr)
	}
	return fmt.Errorf("latency WebTransport did not become ready")
}

// verifyThroughputWebTransport proves a session can be established. bytes=0
// asks the server to serve nothing.
func verifyThroughputWebTransport(ctx context.Context, cfg Config, target *wire.ThroughputTarget) error {
	verifyCtx, cancel := context.WithTimeout(ctx, 3*time.Second)
	defer cancel()
	sess, err := wtDial(verifyCtx, cfg, target.Origin, target.Routes.WTDownload, url.Values{"bytes": {"0"}})
	if err != nil {
		return err
	}
	sess.close()
	return nil
}

// wtBus carries the wire protocol on session datagrams, so a ping that never
// returns is packet loss rather than a stalled queue.
type wtBus struct{ sess *wtSession }

func (b wtBus) Send(_ context.Context, msg string) error { return b.sess.SendDatagram([]byte(msg)) }

func (b wtBus) Recv(ctx context.Context) (string, error) {
	data, err := b.sess.ReceiveDatagram(ctx)
	return string(data), err
}

func (b wtBus) Close() { b.sess.close() }

// wtRedialBackoff paces session re-dials, mirroring the fetch lanes' retry
// cadence without spinning on a hard-down server.
const wtRedialBackoff = 500 * time.Millisecond

// wtVerifyReplyTimeout bounds one hello's wait inside the verification window,
// leaving room for retries: the datagram carrying it may simply be lost.
const wtVerifyReplyTimeout = 750 * time.Millisecond

// wtStageSession owns one stage's session and re-dials it when it drops, so a
// stage outlasting the server's session lifetime continues on a fresh session
// the way fetch lanes continue on fresh requests. establish, when set, runs
// once per new session before any lane sees it.
type wtStageSession struct {
	dial      func(ctx context.Context) (*wtSession, error)
	establish func(ctx context.Context, sess *wtSession) error
	mu        sync.Mutex
	sess      *wtSession
	gen       int
}

func newWTStageSession(ctx context.Context, dial func(ctx context.Context) (*wtSession, error), establish func(ctx context.Context, sess *wtSession) error) (*wtStageSession, error) {
	w := &wtStageSession{dial: dial, establish: establish}
	sess, err := dial(ctx)
	if err != nil {
		return nil, err
	}
	if establish != nil {
		if err := establish(ctx, sess); err != nil {
			sess.close()
			return nil, err
		}
	}
	w.sess = sess
	return w, nil
}

// current returns the live session and the generation to hand redial after a
// failure on it.
func (w *wtStageSession) current() (*wtSession, int) {
	w.mu.Lock()
	defer w.mu.Unlock()
	return w.sess, w.gen
}

// redial replaces generation gen. Lanes share one session, so every lane races
// here when it dies; the first replaces it and the rest adopt the replacement.
// Dial failures retry until ctx ends: mid-run outages are the stall the
// measured pause already prices in, exactly as on the fetch path.
func (w *wtStageSession) redial(ctx context.Context, gen int) error {
	w.mu.Lock()
	defer w.mu.Unlock()
	if w.gen > gen {
		return nil
	}
	w.sess.close()
	for {
		dialCtx, cancel := context.WithTimeout(ctx, 3*time.Second)
		sess, err := w.dial(dialCtx)
		if err == nil && w.establish != nil {
			if err = w.establish(dialCtx, sess); err != nil {
				sess.close()
			}
		}
		cancel()
		if err == nil {
			w.sess = sess
			w.gen++
			return nil
		}
		select {
		case <-ctx.Done():
			return ctx.Err()
		case <-time.After(wtRedialBackoff):
		}
	}
}

func (w *wtStageSession) close() {
	w.mu.Lock()
	defer w.mu.Unlock()
	w.sess.close()
}

// wtLaneMaxFastFailures is how many back-to-back instant failures a lane
// absorbs before it reports one. A server refusing at capacity resets every
// lane it is handed, which must fail the stage rather than be re-dialed for
// the whole measurement window.
const wtLaneMaxFastFailures = 5

// runWTLane keeps one lane alive across session replacements: run the lane on
// the current session, and when it dies with the stage still running, redial
// and continue.
func runWTLane(ctx context.Context, host *wtStageSession, lane func(ctx context.Context, sess *wtSession) error) error {
	fastFailures := 0
	for ctx.Err() == nil {
		sess, gen := host.current()
		started := time.Now()
		err := lane(ctx, sess)
		if err == nil || ctx.Err() != nil {
			return nil
		}
		// A session that ran before it died reconnects without a pause; one
		// that died as fast as it dialled is paced, and is making no progress.
		if time.Since(started) >= wtRedialBackoff {
			fastFailures = 0
		} else {
			if fastFailures++; fastFailures >= wtLaneMaxFastFailures {
				return err
			}
			if !laneRetryPause(ctx) {
				return nil
			}
		}
		if redialErr := host.redial(ctx, gen); redialErr != nil {
			return laneStopError(ctx, redialErr)
		}
	}
	return nil
}

// wtDownloadQuery names what the session serves; the server opens the streams.
func (r *runner) wtDownloadQuery() url.Values {
	return url.Values{
		"bytes":   {strconv.FormatInt(r.cfg.DownloadBytesPerStream, 10)},
		"streams": {strconv.Itoa(r.streams)},
	}
}

// downloadLaneWT accepts and drains the server-opened streams. Every lane runs
// the same accept loop, so a replaced stream lands on whichever lane is free.
// It returns an error when the session dies with the stage still running, the
// signal runWTLane redials on.
func (r *runner) downloadLaneWT(ctx context.Context, sess *wtSession, total *atomic.Uint64) error {
	buf := make([]byte, 1024*1024)
	for ctx.Err() == nil {
		str, err := sess.AcceptUniStream(ctx)
		if err != nil {
			return laneStopError(ctx, err)
		}
		stopOnCancel := transport.UnblockReadsOnDone(ctx, str)
		stopOnGone := transport.UnblockReadsOnDone(sess.Context(), str)
		for {
			n, readErr := str.Read(buf)
			if n > 0 {
				total.Add(uint64(n))
			}
			if readErr != nil {
				break
			}
		}
		stopOnCancel()
		stopOnGone()
		if sess.Context().Err() != nil {
			return laneStopError(ctx, sess.Context().Err())
		}
	}
	return nil
}

// uploadLaneWT writes the cycling block on one unidirectional stream while the
// session lives: the stream's own flow control paces it. A dead session
// surfaces as an error, the signal runWTLane redials on.
func (r *runner) uploadLaneWT(ctx context.Context, sess *wtSession, block []byte) error {
	str, err := sess.OpenUniStreamSync(ctx)
	if err != nil {
		return laneStopError(ctx, err)
	}
	defer str.Close() //nolint:errcheck // the stage is over either way
	defer transport.UnblockWritesOnDone(ctx, str)()
	defer transport.UnblockWritesOnDone(sess.Context(), str)()
	// The block is already the payload: a lane with no length limit writes it
	// unchanged, so there is nothing for a cycling body to assemble.
	for ctx.Err() == nil {
		if _, err := str.Write(block); err != nil {
			return laneStopError(ctx, err)
		}
	}
	return nil
}

// acceptUploadProgressWT reads the one stream the server opens on an upload
// session as the progress feed, so the counter rides the connection under test.
func acceptUploadProgressWT(ctx context.Context, sess *wtSession) (*webtransport.ReceiveStream, error) {
	acceptCtx, cancel := context.WithTimeout(ctx, 3*time.Second)
	defer cancel()
	str, err := sess.AcceptUniStream(acceptCtx)
	if err != nil {
		return nil, fmt.Errorf("upload progress stream: %w", err)
	}
	return str, nil
}

// wtProgressStream gives the receive stream the Close a blocked reader needs.
// The deadline poke unblocks a read parked waiting on a session close that may
// never arrive.
type wtProgressStream struct{ *webtransport.ReceiveStream }

func (s wtProgressStream) Close() error {
	s.CancelRead(0)
	_ = s.SetReadDeadline(time.Now())
	return nil
}

// laneStopError reports a cancelled stage as a clean stop rather than a failure.
func laneStopError(ctx context.Context, err error) error {
	if ctx.Err() != nil {
		return nil
	}
	return err
}
