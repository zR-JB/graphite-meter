package goclient

import (
	"context"
	"crypto/tls"
	"fmt"
	"net/url"
	"strconv"
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
// stream has no URL of its own, so the CONNECT URL speaks for the session.
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

// wtDownloadQuery names what the session serves; the server opens the streams.
func (r *runner) wtDownloadQuery() url.Values {
	return url.Values{
		"bytes":   {strconv.FormatInt(r.cfg.DownloadBytesPerStream, 10)},
		"streams": {strconv.Itoa(r.streams)},
	}
}

// downloadLaneWT accepts and drains the server-opened streams. Every lane runs
// the same accept loop, so a replaced stream lands on whichever lane is free.
func (r *runner) downloadLaneWT(ctx context.Context, sess *wtSession, total *atomic.Uint64) error {
	buf := make([]byte, 1024*1024)
	for ctx.Err() == nil {
		str, err := sess.AcceptUniStream(ctx)
		if err != nil {
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

// openUploadProgressWT reads the NDJSON feed off the one stream the server
// opens on an upload session, so the counter rides the connection under test.
func (r *runner) openUploadProgressWT(ctx context.Context, sess *wtSession, id string) (*uploadProgress, error) {
	acceptCtx, cancel := context.WithTimeout(ctx, 3*time.Second)
	defer cancel()
	str, err := sess.AcceptUniStream(acceptCtx)
	if err != nil {
		return nil, fmt.Errorf("upload progress stream: %w", err)
	}
	base, err := r.endpoint(r.routes().UploadProgress)
	if err != nil {
		return nil, err
	}
	return r.readUploadProgress(ctx, wtProgressStream{str}, withUploadID(base, id))
}

// wtProgressStream gives the receive stream the Close a blocked reader needs.
type wtProgressStream struct{ *webtransport.ReceiveStream }

func (s wtProgressStream) Close() error {
	s.CancelRead(0)
	return nil
}

// laneStopError reports a cancelled stage as a clean stop rather than a failure.
func laneStopError(ctx context.Context, err error) error {
	if ctx.Err() != nil {
		return nil
	}
	return err
}
