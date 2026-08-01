package goclient

import (
	"context"
	"crypto/tls"
	"errors"
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

// wtSession is a dialed WebTransport session and the transport that owns it.
type wtSession struct {
	*webtransport.Session
	transport *webtransport.Transport
	// lifetime ends when the session does. It is the session's own context,
	// captured at dial rather than read back through the embedded session: the
	// retry decisions below have to hold for a session that was never dialled,
	// and the embedded value answers Context with a nil context until quic-go
	// has built one.
	lifetime context.Context
	closed   atomic.Bool
}

// close releases the session and the transport that owns it, once. Whichever of
// the stage host, the replacement dial, or the lane that found it dead gets
// there first does the work; quic-go takes a lock on the way through, so the
// guards also keep a session this client never dialled from panicking there.
func (s *wtSession) close() {
	if s == nil || !s.closed.CompareAndSwap(false, true) {
		return
	}
	if s.Session != nil {
		_ = s.CloseWithError(0, "")
	}
	if s.transport != nil {
		_ = s.transport.Close()
	}
}

// alive reports whether the session can still carry a stream. A lane error on a
// live session is a stream-level fault that lane retries by itself; only a dead
// session is worth replacing, and replacing one that sibling lanes are still
// transferring on stops every one of them.
func (s *wtSession) alive() bool {
	return s != nil && !s.closed.Load() && s.lifetime != nil && s.lifetime.Err() == nil
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
	wtTransport := &webtransport.Transport{
		TLSClientConfig: &tls.Config{InsecureSkipVerify: cfg.InsecureSkipTLSVerify}, //nolint:gosec
		QUICConfig:      transport.NewQUICConfig(),
	}
	_, sess, err := wtTransport.Dial(ctx, u, hdr)
	if err != nil {
		_ = wtTransport.Close()
		return nil, fmt.Errorf("webtransport dial %s: %w", u, err)
	}
	return &wtSession{Session: sess, transport: wtTransport, lifetime: sess.Context()}, nil
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

// wtSessionRedialWindow bounds one session replacement, for the reason
// busRedialWindow bounds the latency bus's: a measured window's clock runs
// whether or not a session is up, so a stage that spends longer than this
// without one is dividing bytes it did move by a window it did not measure over.
// Past it the stage fails instead of reporting that quotient as a rate.
const wtSessionRedialWindow = busRedialWindow

// errWTStageClosed answers a lane still unwinding after its stage tore the
// session host down. The stage is over; nothing may dial behind it.
var errWTStageClosed = errors.New("webtransport stage session closed")

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
	closed    bool
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
// Dial failures retry within wtSessionRedialWindow and then give up: a stage
// with no session is not measuring, and the window it reports over keeps
// running regardless, so a longer outage has to fail the stage rather than be
// priced into it.
func (w *wtStageSession) redial(ctx context.Context, gen int) error {
	w.mu.Lock()
	defer w.mu.Unlock()
	if w.closed {
		return errWTStageClosed
	}
	if w.gen > gen {
		return nil
	}
	// The dead session stays in place until a replacement lands: a sibling lane
	// reading it finds it closed and asks for a replacement of its own, which is
	// the same answer, whereas a nil would be a crash.
	w.sess.close()
	windowCtx, cancelWindow := context.WithTimeout(ctx, wtSessionRedialWindow)
	defer cancelWindow()
	var lastErr error
	for {
		// The window is the whole bound an attempt gets. A separate per-attempt
		// timeout would have to be shorter than the window to be reachable and
		// longer than a handshake to be usable, and the window is already both:
		// an attempt that outlives it fails with it, which is the same answer.
		sess, err := w.dial(windowCtx)
		if err == nil && w.establish != nil {
			if err = w.establish(windowCtx, sess); err != nil {
				sess.close()
			}
		}
		if err == nil {
			w.sess = sess
			w.gen++
			return nil
		}
		lastErr = err
		select {
		case <-windowCtx.Done():
			// The stage's own cancellation is a stop, not a failure; the window
			// expiring while the stage still wants bytes is the failure.
			if ctx.Err() != nil {
				return ctx.Err()
			}
			return fmt.Errorf("webtransport session lost and not replaced within %v: %w", wtSessionRedialWindow, lastErr)
		case <-time.After(wtRedialBackoff):
		}
	}
}

func (w *wtStageSession) close() {
	w.mu.Lock()
	defer w.mu.Unlock()
	w.closed = true
	w.sess.close()
}

// wtLaneMaxFastFailures is the run of back-to-back instant failures that fails a
// lane: every one before the last is absorbed as a retry, and the last is
// reported. A server refusing at capacity resets every lane it is handed, which
// must fail the stage rather than be re-dialed for the whole measurement window.
const wtLaneMaxFastFailures = 5

// wtLaneProgressWindow bounds consecutive failed lane attempts that report no
// successful I/O. Their individual durations do not prove progress: an open,
// accept, read, or write can block for the whole window and still fail before a
// byte moves. It is wtSessionRedialWindow because both answer the same question:
// how long the measured clock may run while this lane carries nothing.
const wtLaneProgressWindow = wtSessionRedialWindow

// runWTLane keeps one lane alive across session replacements: run the lane on
// the current session, and when it dies with the stage still running, redial
// and continue.
func runWTLane(ctx context.Context, host *wtStageSession, lane func(ctx context.Context, sess *wtSession) (bool, error)) error {
	fastFailures := 0
	var failingSince time.Time
	for ctx.Err() == nil {
		sess, gen := host.current()
		started := time.Now()
		progressed, err := lane(ctx, sess)
		if err == nil || ctx.Err() != nil {
			return nil
		}
		// Only successful I/O proves a lane was measuring. How long an open,
		// accept, read, or write blocked before failing says nothing about whether
		// it carried a byte. Real progress clears both failure bounds; a lane that
		// stays healthy never returns here and therefore needs no elapsed-time
		// proxy.
		if progressed {
			fastFailures = 0
			failingSince = time.Time{}
		} else {
			// A session that ran before it died reconnects without a pause; one
			// that died as fast as it dialled is paced, and is making no progress.
			if time.Since(started) >= wtRedialBackoff {
				fastFailures = 0
				if failingSince.IsZero() {
					failingSince = started
				}
				// Whatever the attempt duration or session liveness, a lane that
				// cannot carry a byte is the same measured shortfall. This catches
				// one long failed operation as well as a mixed run of shorter ones.
				if time.Since(failingSince) >= wtLaneProgressWindow {
					return err
				}
			} else {
				if fastFailures++; fastFailures >= wtLaneMaxFastFailures {
					return err
				}
				if !laneRetryPause(ctx) {
					return nil
				}
			}
		}
		// A lane's own stream failing is not a lost session: the siblings sharing
		// it are still transferring, and replacing it to serve one lane's retry
		// stops every one of them mid-stream. Only a session that is actually
		// gone is replaced; anything else is this lane's to retry.
		if sess.alive() {
			continue
		}
		if redialErr := host.redial(ctx, gen); redialErr != nil {
			// A stage the caller cancelled, or one whose own window ended, is a
			// clean stop. Anything else means the session stayed down while the
			// measured window kept running: the bytes stop and the clock does
			// not, so reporting nil here publishes the shortfall as a rate.
			if ctx.Err() != nil {
				return nil
			}
			return redialErr
		}
	}
	return nil
}

// wtDownloadQuery names what the session serves; the server opens the streams.
func (r *runner) wtDownloadQuery() url.Values {
	return url.Values{
		"bytes":   {strconv.FormatInt(r.cfg.DownloadBytesPerStream, 10)},
		"streams": {strconv.Itoa(r.streams.of(Down))},
	}
}

// downloadLaneWT accepts and drains the server-opened streams. Every lane runs
// the same accept loop, so a replaced stream lands on whichever lane is free.
// It returns an error when the session dies with the stage still running, the
// signal runWTLane redials on.
func (r *runner) downloadLaneWT(ctx context.Context, sess *wtSession, total *atomic.Uint64) (bool, error) {
	buf := make([]byte, 1024*1024)
	progressed := false
	for ctx.Err() == nil {
		str, err := sess.AcceptUniStream(ctx)
		if err != nil {
			return progressed, laneStopError(ctx, err)
		}
		stopOnCancel := transport.UnblockReadsOnDone(ctx, str)
		stopOnGone := transport.UnblockReadsOnDone(sess.Context(), str)
		for {
			n, readErr := str.Read(buf)
			if n > 0 {
				progressed = true
				total.Add(uint64(n))
			}
			if readErr != nil {
				break
			}
		}
		stopOnCancel()
		stopOnGone()
		if sess.Context().Err() != nil {
			return progressed, laneStopError(ctx, sess.Context().Err())
		}
	}
	return progressed, nil
}

// uploadLaneWT writes the cycling block on one unidirectional stream while the
// session lives: the stream's own flow control paces it. A dead session
// surfaces as an error, the signal runWTLane redials on.
func (r *runner) uploadLaneWT(ctx context.Context, sess *wtSession, block []byte) (bool, error) {
	str, err := sess.OpenUniStreamSync(ctx)
	if err != nil {
		return false, laneStopError(ctx, err)
	}
	defer str.Close() //nolint:errcheck // the stage is over either way
	defer transport.UnblockWritesOnDone(ctx, str)()
	defer transport.UnblockWritesOnDone(sess.Context(), str)()
	// The block is already the payload: a lane with no length limit writes it
	// unchanged, so there is nothing for a cycling body to assemble.
	progressed := false
	for ctx.Err() == nil {
		n, err := str.Write(block)
		if n > 0 {
			progressed = true
		}
		if err != nil {
			return progressed, laneStopError(ctx, err)
		}
	}
	return progressed, nil
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
