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

// SessionHandler serves one accepted WebTransport session until it ends. The
// adapter owns the session: ctx ends with it, and the session closes once the handler returns.
type SessionHandler func(ctx context.Context, sess *webtransport.Session, r *http.Request)

// datagramConn is the datagram half of a WebTransport session.
type datagramConn interface {
	SendDatagram(b []byte) error
	ReceiveDatagram(ctx context.Context) ([]byte, error)
}

const wtDatagramPayload = 1000

// A verify session's answer is its handshake: both clients close it at once,
// so an abandoned one holds its session slot only briefly.
const wtVerifyLinger = 5 * time.Second

// wtRefusalLinger lets a peer read a refusal record before the session closes under it.
const wtRefusalLinger = 2 * time.Second

// sessionActivity ends a session that carried no peer activity for about its idle bound.
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

// WTPing serves the latency bus over session datagrams, which measure application probe timeouts.
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
func WTDownload(stream StreamFunc, idleBound time.Duration) SessionHandler {
	return func(ctx context.Context, sess *webtransport.Session, r *http.Request) {
		query := r.URL.Query()
		n := parseBytes(query.Get("bytes"))
		// Parse rather than compare spellings: any zero request serves nothing.
		if n == 0 {
			lingerForPeer(ctx, sess, wtVerifyLinger)
			return
		}
		ctx, live := watchSession(ctx, idleBound)
		var wg sync.WaitGroup
		defer wg.Wait()
		defer live.cancel()
		if wtDatagramMode(query) {
			// The flood is this server's own traffic, so it cannot be what keeps the session alive.
			wg.Go(func() { bumpOnPeerDatagrams(ctx, sess, live) })
			sink := &datagramSink{conn: sess, done: ctx.Done()}
			for ctx.Err() == nil && !sink.failed {
				_ = stream(ctx, n, sink)
			}
			return
		}
		lanes := laneOpener(func(ctx context.Context) (laneStream, error) {
			return sess.OpenUniStreamSync(ctx)
		})
		for range wtStreamCount(query) {
			wg.Go(func() { serveDownloadLane(ctx, stream, lanes, n, live) })
		}
		// The session lasts as long as any lane is being served.
		wg.Wait()
	}
}

type laneStream interface {
	io.WriteCloser
	CancelWrite(webtransport.StreamErrorCode)
	SetWriteDeadline(time.Time) error
}

type laneOpener func(context.Context) (laneStream, error)

// serveDownloadLane replaces each exhausted lane for as long as the peer keeps draining them.
func serveDownloadLane(ctx context.Context, stream StreamFunc, lanes laneOpener, n int64, live *sessionActivity) {
	for ctx.Err() == nil {
		str, err := lanes(ctx)
		if err != nil {
			return
		}
		lane := &laneWriter{w: str, live: live}
		withWTWriteStream(ctx, str, func() { _ = stream(ctx, n, lane) })
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

func withWTWriteStream(ctx context.Context, str laneStream, serve func()) {
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

// WTUpload drains client-opened streams as upload lanes into the session's
// receiver and serves its progress feed on one server-opened stream. receive
// counts each lane; it is upload.Receive unless a test observes the lanes.
func WTUpload(upload *Upload, receive ReceiveFunc, idleBound time.Duration) SessionHandler {
	return func(ctx context.Context, sess *webtransport.Session, r *http.Request) {
		query := r.URL.Query()
		id := query.Get("id")
		// A stream carries no request, so its CONNECT identifies the upload owner.
		owner := UploadOwner(r, upload.trusted)
		agg, access := upload.store.watchFor(id, owner)
		if access != uploadAccessOK {
			// The refusal is the whole answer, so the session and its admission slot end with it.
			serveRefusal(ctx, sess, access)
			lingerForPeer(ctx, sess, wtRefusalLinger)
			return
		}
		// The progress feed is server-generated, so its heartbeat must not count as activity.
		ctx, live := watchSession(ctx, idleBound)
		// Every goroutine below ends with the session's context, and the session ends only once they have.
		var wg sync.WaitGroup
		defer wg.Wait()
		defer live.cancel()
		wg.Go(func() {
			str, err := sess.OpenUniStreamSync(ctx)
			if err == nil {
				withWTWriteStream(ctx, str, func() { streamProgress(ctx, agg, str) })
			}
		})
		if wtDatagramMode(query) {
			wg.Go(func() { drainDatagrams(ctx, receive, sess, agg, id, owner, live) })
		}
		// The client opens these, so the ceiling the download side applies to its own lanes applies here too.
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
					serveUploadLane(ctx, receive, sess, str, id, owner, live)
				})
			default:
				str.CancelRead(0)
			}
		}
	}
}

func serveUploadLane(ctx context.Context, receive ReceiveFunc, sess *webtransport.Session, str *webtransport.ReceiveStream, id, owner string, live *sessionActivity) {
	// A blocked read watches neither the session's end nor its idle bound.
	defer transport.UnblockReadsOnDone(ctx, str)()
	_, err := receive(ctx, id, owner, &idleTimeoutReader{str: str, timeout: uploadReadTimeout, live: live})
	if refusal, ok := errors.AsType[*uploadRefusalError](err); ok {
		// Stream uploads have no response headers.
		serveRefusal(ctx, sess, refusal.access)
	}
	// Whatever ended the lane — a refusal, the idle bound, or a clean end — the stream is reset.
	str.CancelRead(0)
}

func serveRefusal(ctx context.Context, sess *webtransport.Session, access uploadAccess) {
	str, err := sess.OpenUniStreamSync(ctx)
	if err != nil {
		return
	}
	withWTWriteStream(ctx, str, func() { writeRefusalRecord(str, access) })
}

func drainDatagrams(ctx context.Context, receive ReceiveFunc, conn datagramConn, agg *uploadAgg, id, owner string, live *sessionActivity) {
	ctx, cancel := context.WithCancel(ctx)
	defer cancel()
	go func() {
		select {
		case <-agg.finished:
			cancel()
		case <-ctx.Done():
		}
	}()
	// The session's idle watcher bounds a silent drain; there is no stream to time out.
	_, _ = receive(ctx, id, owner, datagramSource{conn: conn, ctx: ctx, live: live})
}

// idleTimeoutReader bounds a lane by inactivity: its read deadline stays
// between 7/8 and all of timeout ahead of the last read.
type idleTimeoutReader struct {
	str     deadlineReader
	timeout time.Duration
	live    *sessionActivity
	armed   time.Time
}

type deadlineReader interface {
	io.Reader
	SetReadDeadline(time.Time) error
}

func (r *idleTimeoutReader) Read(p []byte) (int, error) {
	// Re-arming costs more than the read it guards, so it happens once per eighth of the timeout.
	if now := time.Now(); now.Sub(r.armed) > r.timeout/8 {
		_ = r.str.SetReadDeadline(now.Add(r.timeout))
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
	// SendDatagram blocks on a full send queue and watches no ctx, so each datagram checks it first.
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
