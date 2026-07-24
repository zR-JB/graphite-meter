package goclient

import (
	"context"
	"crypto/tls"
	"fmt"
	"io"
	"net/url"
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

// wtDial opens a session on origin's path, carrying query as the session URL's
// parameters: a stream has no URL of its own.
func wtDial(ctx context.Context, cfg Config, origin, path string, query url.Values) (*wtSession, error) {
	u, err := httpEndpoint(origin, path)
	if err != nil {
		return nil, err
	}
	if len(query) > 0 {
		u += "?" + query.Encode()
	}
	dialer := &webtransport.Dialer{
		TLSClientConfig: &tls.Config{InsecureSkipVerify: cfg.InsecureSkipTLSVerify}, //nolint:gosec
		QUICConfig:      transport.NewQUICConfig(),
	}
	_, sess, err := dialer.Dial(ctx, u, nil)
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
	if err := sess.SendDatagram([]byte(wire.Encode(wire.Frame{Op: wire.OpHI, Proto: "wt"}))); err != nil {
		return fmt.Errorf("latency WebTransport hello failed: %w", err)
	}
	reply, err := sess.ReceiveDatagram(verifyCtx)
	if err != nil {
		return fmt.Errorf("latency WebTransport readiness failed: %w", err)
	}
	if frame, err := wire.Decode(string(reply)); err != nil || frame.Op != wire.OpREADY {
		return fmt.Errorf("latency WebTransport did not become ready")
	}
	return nil
}

// verifyThroughputWebTransport proves a download session can be opened, so
// automatic selection can fall back before the run starts.
func verifyThroughputWebTransport(ctx context.Context, cfg Config, target *wire.ThroughputTarget) error {
	verifyCtx, cancel := context.WithTimeout(ctx, 3*time.Second)
	defer cancel()
	sess, err := wtDial(verifyCtx, cfg, target.Origin, target.Routes.WTDownload, nil)
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

// downloadLaneWT runs one lane on its own bidirectional stream: the SIZE
// preamble sizes the response, and an exhausted stream is reopened.
func (r *runner) downloadLaneWT(ctx context.Context, sess *wtSession, total *atomic.Uint64) error {
	preamble := wire.EncodeStreamPreamble(wire.Frame{Op: wire.OpSIZE, Bytes: uint64(r.cfg.DownloadBytesPerStream)}) //nosec G115 -- configured size is non-negative
	buf := make([]byte, 1024*1024)
	for ctx.Err() == nil {
		str, err := sess.OpenStreamSync(ctx)
		if err != nil {
			return laneStopError(ctx, err)
		}
		if _, err := io.WriteString(str, preamble); err != nil {
			return laneStopError(ctx, err)
		}
		// A blocked stream read does not observe the context on its own.
		stopOnCancel := context.AfterFunc(ctx, func() { str.CancelRead(0) })
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
		_ = str.Close()
	}
	return nil
}

// uploadLaneWT writes the cycling block on one unidirectional stream for the
// whole stage: the stream's own flow control paces it.
func (r *runner) uploadLaneWT(ctx context.Context, sess *wtSession, block []byte) error {
	str, err := sess.OpenUniStreamSync(ctx)
	if err != nil {
		return laneStopError(ctx, err)
	}
	defer str.Close() //nolint:errcheck // the stage is over either way
	// A write blocked on flow control does not observe the context on its own.
	defer context.AfterFunc(ctx, func() { str.CancelWrite(0) })()
	body := &cyclingBody{ctx: ctx, block: block}
	buf := make([]byte, len(block))
	for ctx.Err() == nil {
		n, err := body.Read(buf)
		if err != nil {
			return laneStopError(ctx, err)
		}
		if _, err := str.Write(buf[:n]); err != nil {
			return laneStopError(ctx, err)
		}
	}
	return nil
}

// openUploadProgressWT reads the same NDJSON feed off a bidirectional stream of
// the upload session, so the counter rides the connection under test.
func (r *runner) openUploadProgressWT(ctx context.Context, sess *wtSession, id string) (*uploadProgress, error) {
	str, err := sess.OpenStreamSync(ctx)
	if err != nil {
		return nil, err
	}
	if _, err := io.WriteString(str, wire.EncodeStreamPreamble(wire.Frame{Op: wire.OpHI, Proto: "wt"})); err != nil {
		return nil, err
	}
	base, err := r.endpoint(r.routes().UploadProgress)
	if err != nil {
		return nil, err
	}
	return r.readUploadProgress(ctx, wtProgressStream{str}, withUploadID(base, id))
}

// wtProgressStream ends its reader on close: closing a stream shuts the write
// side, while a blocked read needs its own cancellation.
type wtProgressStream struct{ *webtransport.Stream }

func (s wtProgressStream) Close() error {
	s.CancelRead(0)
	return s.Stream.Close()
}

// laneStopError reports a cancelled stage as a clean stop rather than a failure.
func laneStopError(ctx context.Context, err error) error {
	if ctx.Err() != nil {
		return nil
	}
	return err
}
