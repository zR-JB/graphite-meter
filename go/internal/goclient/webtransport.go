package goclient

import (
	"context"
	"crypto/tls"
	"fmt"
	"github.com/zR-JB/graphite-meter/go/internal/route"
	"io"
	"net/http"
	"net/url"
	"strconv"
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
	if token := cfg.grant; token != "" {
		parsed, err := url.Parse(u)
		if err != nil || cfg.InsecureSkipTLSVerify || !grantAllowed(parsed, cfg) {
			return nil, fmt.Errorf("refusing to send authentication grant outside the server's HTTPS origins")
		}
		hdr = http.Header{"Authorization": {"Bearer " + token}}
	}
	wtTransport := &webtransport.Transport{
		TLSClientConfig: &tls.Config{InsecureSkipVerify: cfg.InsecureSkipTLSVerify}, //nolint:gosec
		QUICConfig:      transport.NewQUICConfig(),
	}
	response, sess, err := wtTransport.Dial(ctx, u, hdr)
	if err != nil {
		_ = wtTransport.Close()
		if authErr := authResponseError(response); authErr != nil {
			return nil, authErr
		}
		return nil, fmt.Errorf("webtransport dial %s: %w", u, err)
	}
	return &wtSession{Session: sess, transport: wtTransport, lifetime: sess.Context()}, nil
}

func verifyThroughputWebTransport(ctx context.Context, cfg Config, target *wire.ThroughputTarget) error {
	verifyCtx, cancel := context.WithTimeout(ctx, 3*time.Second)
	defer cancel()
	sess, err := wtDial(verifyCtx, cfg, target.Origin, route.WTDownload, url.Values{"bytes": {"0"}})
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

type wtStageSession struct {
	dial      func(ctx context.Context) (*wtSession, error)
	establish func(ctx context.Context, sess *wtSession) error
	mu        sync.Mutex
	sess      *wtSession
	gen       int
}

func newWTStageSession(
	ctx context.Context,
	dial func(ctx context.Context) (*wtSession, error),
	establish func(ctx context.Context, sess *wtSession) error,
) (*wtStageSession, error) {
	w := &wtStageSession{dial: dial, establish: establish}
	sess, err := w.open(ctx)
	if err != nil {
		return nil, err
	}
	w.sess = sess
	return w, nil
}

func (w *wtStageSession) open(ctx context.Context) (*wtSession, error) {
	sess, err := w.dial(ctx)
	if err != nil || w.establish == nil {
		return sess, err
	}
	if err := w.establish(ctx, sess); err != nil {
		sess.close()
		return nil, err
	}
	return sess, nil
}

func (w *wtStageSession) current() (*wtSession, int) {
	w.mu.Lock()
	defer w.mu.Unlock()
	return w.sess, w.gen
}

// redial holds the lock while dialling so other lanes wait for the replacement instead of the dead session.
func (w *wtStageSession) redial(ctx context.Context, gen int) error {
	w.mu.Lock()
	defer w.mu.Unlock()
	if w.gen > gen {
		return nil
	}
	w.sess.close()
	return restore(ctx, time.Now().Add(redialWindow), "webtransport session", func(ctx context.Context) error {
		sess, err := w.open(ctx)
		if err == nil {
			w.sess = sess
			w.gen++
		}
		return err
	})
}

// close runs after every lane has stopped.
func (w *wtStageSession) close() { w.sess.close() }

func runWTLane(
	ctx context.Context,
	host *wtStageSession,
	lane func(ctx context.Context, sess *wtSession) (bool, error),
) error {
	return persist(ctx, func(ctx context.Context) (bool, error) {
		sess, gen := host.current()
		if !sess.alive() {
			if err := host.redial(ctx, gen); err != nil {
				return false, err
			}
			sess, _ = host.current()
		}
		return lane(ctx, sess)
	})
}

func (r *runner) wtDownloadQuery() url.Values {
	return url.Values{
		"bytes":   {strconv.FormatInt(transferBytesPerStream, 10)},
		"streams": {strconv.Itoa(r.streams.of(Down))},
	}
}

func downloadLaneWT(
	ctx context.Context,
	sess *wtSession,
	buf []byte,
	total *atomic.Uint64,
	ready func(),
) (bool, error) {
	progressed := false
	for ctx.Err() == nil {
		str, err := sess.AcceptUniStream(ctx)
		if err != nil {
			return progressed, laneStopError(ctx, err)
		}
		ready()
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

func uploadLaneWT(ctx context.Context, sess *wtSession, block []byte, ready func()) (bool, error) {
	str, err := sess.OpenUniStreamSync(ctx)
	if err != nil {
		return false, laneStopError(ctx, err)
	}
	ready()
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

func wtProgressFeed(lifetime context.Context, stream *webtransport.ReceiveStream) io.ReadCloser {
	interrupt := func() {
		stream.CancelRead(0)
		_ = stream.SetReadDeadline(time.Now())
	}
	stop := context.AfterFunc(lifetime, interrupt)
	return progressFeed{stream, func() { stop(); interrupt() }}
}

func laneStopError(ctx context.Context, err error) error {
	if ctx.Err() != nil {
		return nil
	}
	return err
}
