package endpoint

import (
	"context"
	"errors"
	"io"
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

// SessionHandler serves one WebTransport session; ctx ends with it and the adapter closes it on return.
type SessionHandler func(ctx context.Context, sess *webtransport.Session, r *http.Request)

type datagramConn interface {
	SendDatagram(b []byte) error
	ReceiveDatagram(ctx context.Context) ([]byte, error)
}

const wtDatagramPayload = 1000

// A verify session's answer is its handshake; clients close it at once.
const wtVerifyLinger = 5 * time.Second

// wtRefusalLinger lets a peer read a refusal before the session closes.
const wtRefusalLinger = 2 * time.Second

// sessionActivity ends a session idle for about its bound.
type sessionActivity struct {
	n      atomic.Uint64
	cancel context.CancelFunc
}

func watchSession(parent context.Context, bound time.Duration) (context.Context, *sessionActivity) {
	ctx, cancel := context.WithCancel(parent)
	a := &sessionActivity{cancel: cancel}
	go a.watch(ctx, bound)
	return ctx, a
}

func (a *sessionActivity) bump() {
	if a != nil {
		a.n.Add(1)
	}
}

func (a *sessionActivity) watch(ctx context.Context, bound time.Duration) {
	tick := time.Tick(bound / 2)
	last := a.n.Load()
	quiet := 0
	for {
		select {
		case <-ctx.Done():
			return
		case <-tick:
			now := a.n.Load()
			if now != last {
				last, quiet = now, 0
				continue
			}
			if quiet++; quiet == 2 {
				a.cancel()
				return
			}
		}
	}
}

func lingerForPeer(ctx context.Context, sess *webtransport.Session, bound time.Duration) {
	timer := time.NewTimer(bound)
	defer timer.Stop()
	select {
	case <-ctx.Done():
	case <-sess.Context().Done():
	case <-timer.C:
	}
}

func WTPing(idleBound time.Duration) SessionHandler {
	return func(ctx context.Context, sess *webtransport.Session, _ *http.Request) {
		ctx, live := watchSession(ctx, idleBound)
		defer live.cancel()
		ServePing(func() ([]byte, error) {
			data, err := sess.ReceiveDatagram(ctx)
			if err == nil {
				live.bump()
			}
			return data, err
		}, sess.SendDatagram)
	}
}

// WTDownload serves byte lanes on server-opened streams, or a datagram flood.
func WTDownload(download *Download, idleBound time.Duration) SessionHandler {
	return func(ctx context.Context, sess *webtransport.Session, r *http.Request) {
		query := r.URL.Query()
		n := parseBytes(query.Get("bytes"))
		// Any spelling of zero is a verify session.
		if n == 0 {
			lingerForPeer(ctx, sess, wtVerifyLinger)
			return
		}
		ctx, live := watchSession(ctx, idleBound)
		var wg sync.WaitGroup
		defer wg.Wait()
		defer live.cancel()
		if wtDatagramMode(query) {
			// Only the peer's datagrams count as activity.
			wg.Go(func() { bumpOnPeerDatagrams(ctx, sess, live) })
			sink := &datagramSink{conn: sess, done: ctx.Done()}
			for ctx.Err() == nil && !sink.failed {
				download.Stream(ctx, n, sink)
			}
			return
		}
		for range wtStreamCount(query) {
			wg.Go(func() { serveDownloadLane(ctx, download, sess, n, live) })
		}
		wg.Wait()
	}
}

// serveDownloadLane replaces each exhausted lane while the peer keeps draining.
func serveDownloadLane(ctx context.Context, download *Download, sess *webtransport.Session, n int64,
	live *sessionActivity) {
	for ctx.Err() == nil {
		str, err := sess.OpenUniStreamSync(ctx)
		if err != nil {
			return
		}
		lane := &laneWriter{w: str, live: live}
		withWTWriteStream(ctx, str, func() { download.Stream(ctx, n, lane) })
		if !lane.moved {
			return
		}
	}
}

type laneWriter struct {
	w     io.Writer
	live  *sessionActivity
	moved bool
}

func (c *laneWriter) Write(p []byte) (int, error) {
	n, err := c.w.Write(p)
	if n > 0 {
		c.moved = true
		c.live.bump()
	}
	return n, err
}

func withWTWriteStream(ctx context.Context, str *webtransport.SendStream, serve func()) {
	defer str.Close()
	defer transport.UnblockWritesOnDone(ctx, str)()
	serve()
}

func wtDatagramMode(query url.Values) bool {
	raw, ok := query["datagrams"]
	if !ok || len(raw) == 0 {
		return false
	}
	value := strings.TrimSpace(raw[0])
	if value == "" {
		return true
	}
	if n, err := strconv.ParseInt(value, 10, 64); err == nil {
		return n != 0
	}
	switch strings.ToLower(value) {
	case "false", "off", "no":
		return false
	}
	return true
}

func wtStreamCount(query url.Values) int {
	n, err := strconv.Atoi(query.Get("streams"))
	if err != nil || n < 1 {
		return 1
	}
	return min(n, wire.WTMaxStreams)
}

// WTUpload counts client-opened lanes and serves the progress feed on one server stream.
func WTUpload(upload *Upload, idleBound time.Duration) SessionHandler {
	return func(ctx context.Context, sess *webtransport.Session, r *http.Request) {
		query := r.URL.Query()
		id := query.Get("id")
		owner := UploadOwner(r, upload.trusted)
		agg, access := upload.accessFor(id, owner, false)
		if access != uploadAccessOK {
			serveRefusal(ctx, sess, access)
			lingerForPeer(ctx, sess, wtRefusalLinger)
			return
		}
		// The feed's heartbeat is not peer activity.
		ctx, live := watchSession(ctx, idleBound)
		// The session closes only after its goroutines end.
		var wg sync.WaitGroup
		defer wg.Wait()
		defer live.cancel()
		wg.Go(func() {
			str, err := sess.OpenUniStreamSync(ctx)
			if err == nil {
				withWTWriteStream(ctx, str, func() { upload.streamProgress(ctx, agg, str) })
			}
		})
		if wtDatagramMode(query) {
			wg.Go(func() { drainDatagrams(ctx, upload, sess, agg, id, owner, live) })
		}
		lanes := make(chan struct{}, wire.WTMaxStreams)
		for {
			str, err := sess.AcceptUniStream(ctx)
			if err != nil {
				return
			}
			select {
			case lanes <- struct{}{}:
				wg.Go(func() {
					defer func() { <-lanes }()
					serveUploadLane(ctx, upload, sess, str, id, owner, live)
				})
			default:
				str.CancelRead(0)
			}
		}
	}
}

func serveUploadLane(ctx context.Context, upload *Upload, sess *webtransport.Session,
	str *webtransport.ReceiveStream, id, owner string, live *sessionActivity) {
	// A blocked read does not watch ctx.
	defer transport.UnblockReadsOnDone(ctx, str)()
	_, err := upload.Receive(ctx, id, owner, &idleTimeoutReader{str: str, timeout: uploadReadTimeout, live: live})
	if refusal, ok := errors.AsType[*uploadRefusalError](err); ok {
		serveRefusal(ctx, sess, refusal.access)
	}
	str.CancelRead(0)
}

func serveRefusal(ctx context.Context, sess *webtransport.Session, access uploadAccess) {
	str, err := sess.OpenUniStreamSync(ctx)
	if err != nil {
		return
	}
	withWTWriteStream(ctx, str, func() { writeRefusalRecord(str, access) })
}

func drainDatagrams(ctx context.Context, upload *Upload, conn datagramConn, agg *uploadAgg, id, owner string,
	live *sessionActivity) {
	ctx, cancel := context.WithCancel(ctx)
	defer cancel()
	go func() {
		select {
		case <-agg.finished:
			cancel()
		case <-ctx.Done():
		}
	}()
	// The session watcher bounds a silent drain.
	_, _ = upload.Receive(ctx, id, owner, datagramSource{conn: conn, ctx: ctx, live: live})
}

// idleTimeoutReader bounds a lane by inactivity, within 7/8 of timeout.
type idleTimeoutReader struct {
	str     deadlineReader
	timeout time.Duration
	limit   time.Time // an absolute bound no re-arming passes, if set
	live    *sessionActivity
	armed   time.Time
}

type deadlineReader interface {
	io.Reader
	SetReadDeadline(time.Time) error
}

func (r *idleTimeoutReader) Read(p []byte) (int, error) {
	if now := time.Now(); now.Sub(r.armed) > r.timeout/8 {
		deadline := now.Add(r.timeout)
		if !r.limit.IsZero() && r.limit.Before(deadline) {
			deadline = r.limit
		}
		_ = r.str.SetReadDeadline(deadline)
		r.armed = now
	}
	n, err := r.str.Read(p)
	if n > 0 {
		r.live.bump()
	}
	return n, err
}

func bumpOnPeerDatagrams(ctx context.Context, conn datagramConn, live *sessionActivity) {
	for {
		if _, err := conn.ReceiveDatagram(ctx); err != nil {
			return
		}
		live.bump()
	}
}

type datagramSink struct {
	conn   datagramConn
	done   <-chan struct{}
	failed bool
}

func (s *datagramSink) Write(p []byte) (int, error) {
	// SendDatagram ignores ctx, so check it per datagram.
	for off := 0; off < len(p); off += wtDatagramPayload {
		select {
		case <-s.done:
			s.failed = true
			return off, context.Canceled
		default:
		}
		if err := s.conn.SendDatagram(p[off:min(off+wtDatagramPayload, len(p))]); err != nil {
			s.failed = true
			return off, err
		}
	}
	return len(p), nil
}

type datagramSource struct {
	conn datagramConn
	ctx  context.Context
	live *sessionActivity
}

// A datagram is delivered whole or not at all: silently dropping its tail would under-report the upload counter.
func (s datagramSource) Read(p []byte) (int, error) {
	data, err := s.conn.ReceiveDatagram(s.ctx)
	if err != nil {
		return 0, err
	}
	if len(data) > len(p) {
		return 0, io.ErrShortBuffer
	}
	s.live.bump()
	return copy(p, data), nil
}
