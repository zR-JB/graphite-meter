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

type wtSession struct {
	*webtransport.Session
	transport *webtransport.Transport
	lifetime  context.Context
	closed    atomic.Bool
}

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

func (s *wtSession) alive() bool {
	return s != nil && !s.closed.Load() && s.lifetime != nil && s.lifetime.Err() == nil
}

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

func verifyLatencyWebTransport(ctx context.Context, cfg Config, target *wire.LatencyTarget) error {
	verifyCtx, cancel := context.WithTimeout(ctx, 3*time.Second)
	defer cancel()
	sess, err := wtDial(verifyCtx, cfg, target.Origin, target.Routes.WTPing, nil)
	if err != nil {
		return err
	}
	defer sess.close()
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

type wtBus struct{ sess *wtSession }

func (b wtBus) Send(_ context.Context, msg string) error { return b.sess.SendDatagram([]byte(msg)) }

func (b wtBus) Recv(ctx context.Context) (string, error) {
	data, err := b.sess.ReceiveDatagram(ctx)
	return string(data), err
}

func (b wtBus) Close() { b.sess.close() }

const wtRedialBackoff = 500 * time.Millisecond

const wtVerifyReplyTimeout = 750 * time.Millisecond

const wtSessionRedialWindow = busRedialWindow

var errWTStageClosed = errors.New("webtransport stage session closed")

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

func (w *wtStageSession) current() (*wtSession, int) {
	w.mu.Lock()
	defer w.mu.Unlock()
	return w.sess, w.gen
}

func (w *wtStageSession) redial(ctx context.Context, gen int) error {
	w.mu.Lock()
	defer w.mu.Unlock()
	if w.closed {
		return errWTStageClosed
	}
	if w.gen > gen {
		return nil
	}
	w.sess.close()
	windowCtx, cancelWindow := context.WithTimeout(ctx, wtSessionRedialWindow)
	defer cancelWindow()
	var lastErr error
	for {
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
		if _, authRequired := errors.AsType[*AuthRequiredError](err); authRequired {
			return err
		}
		if !errors.Is(err, context.DeadlineExceeded) || lastErr == nil {
			lastErr = err
		}
		select {
		case <-windowCtx.Done():
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

const wtLaneMaxFastFailures = 5

const wtLaneProgressWindow = wtSessionRedialWindow

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
		if progressed {
			fastFailures = 0
			failingSince = time.Time{}
		} else if time.Since(started) >= wtRedialBackoff {
			fastFailures = 0
			if failingSince.IsZero() {
				failingSince = started
			}
			if time.Since(failingSince) >= wtLaneProgressWindow {
				return err
			}
		} else if fastFailures++; fastFailures >= wtLaneMaxFastFailures {
			return err
		} else if !laneRetryPause(ctx) {
			return nil
		}
		if sess.alive() {
			continue
		}
		if redialErr := host.redial(ctx, gen); redialErr != nil {
			if ctx.Err() != nil {
				return nil
			}
			return redialErr
		}
	}
	return nil
}

func (r *runner) wtDownloadQuery() url.Values {
	return url.Values{
		"bytes":   {strconv.FormatInt(r.cfg.DownloadBytesPerStream, 10)},
		"streams": {strconv.Itoa(r.streams.of(Down))},
	}
}

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

func (r *runner) uploadLaneWT(ctx context.Context, sess *wtSession, block []byte) (bool, error) {
	str, err := sess.OpenUniStreamSync(ctx)
	if err != nil {
		return false, laneStopError(ctx, err)
	}
	defer str.Close() //nolint:errcheck // the stage is over either way
	defer transport.UnblockWritesOnDone(ctx, str)()
	defer transport.UnblockWritesOnDone(sess.Context(), str)()
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

func acceptUploadProgressWT(ctx context.Context, sess *wtSession) (*webtransport.ReceiveStream, error) {
	acceptCtx, cancel := context.WithTimeout(ctx, 3*time.Second)
	defer cancel()
	str, err := sess.AcceptUniStream(acceptCtx)
	if err != nil {
		return nil, fmt.Errorf("upload progress stream: %w", err)
	}
	return str, nil
}

type wtProgressStream struct{ *webtransport.ReceiveStream }

func (s wtProgressStream) Close() error {
	s.CancelRead(0)
	_ = s.SetReadDeadline(time.Now())
	return nil
}

func laneStopError(ctx context.Context, err error) error {
	if ctx.Err() != nil {
		return nil
	}
	return err
}
