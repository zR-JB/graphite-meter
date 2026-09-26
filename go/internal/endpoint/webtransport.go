package endpoint

import (
	"context"
	"errors"
	"io"
	"net/http"
	"net/netip"
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

// WTHandler serves one accepted WebTransport session until it ends.
type WTHandler interface {
	HandleSession(ctx context.Context, sess *webtransport.Session, r *http.Request)
}

const wtDatagramPayload = 1000

// A verify session's answer is its handshake: both clients close it at once,
// so an abandoned one holds its session slot only briefly.
const wtVerifyLinger = 5 * time.Second

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

type wtPing struct {
	ping      MessageHandler
	idleBound time.Duration
}

// NewWTPing serves the latency bus over session datagrams, which measure application probe timeouts.
func NewWTPing(ping MessageHandler, idleBound time.Duration) WTHandler {
	return &wtPing{ping: ping, idleBound: idleBound}
}

func (h *wtPing) HandleSession(ctx context.Context, sess *webtransport.Session, r *http.Request) {
	ctx, live := watchSession(ctx, h.idleBound)
	bus := transport.NewWebTransportBus(ctx, liveDatagramConn{conn: sess, live: live})
	_ = h.ping.HandleMessages(ctx, bus)
}

type liveDatagramConn struct {
	conn transport.DatagramConn
	live *sessionActivity
}

func (c liveDatagramConn) SendDatagram(b []byte) error { return c.conn.SendDatagram(b) }

func (c liveDatagramConn) ReceiveDatagram(ctx context.Context) ([]byte, error) {
	data, err := c.conn.ReceiveDatagram(ctx)
	if err == nil {
		c.live.bump()
	}
	return data, err
}

type wtDownload struct {
	download  DownloadHandler
	idleBound time.Duration
}

// NewWTDownload serves byte lanes on server-opened WebTransport streams.
func NewWTDownload(download DownloadHandler, idleBound time.Duration) WTHandler {
	return &wtDownload{download: download, idleBound: idleBound}
}

func (h *wtDownload) HandleSession(ctx context.Context, sess *webtransport.Session, r *http.Request) {
	query := r.URL.Query()
	// Parse rather than compare spellings: any zero request serves nothing.
	if parseBytes(query.Get("bytes")) == 0 {
		linger := time.NewTimer(wtVerifyLinger)
		defer linger.Stop()
		select {
		case <-ctx.Done():
		case <-linger.C:
		}
		return
	}
	ctx, live := watchSession(ctx, h.idleBound)
	var wg sync.WaitGroup
	defer wg.Wait()
	defer live.cancel()
	if wtDatagramMode(query) {
		// The flood is this server's own traffic, so it cannot be what keeps the session alive.
		wg.Go(func() { bumpOnPeerDatagrams(ctx, sess, live) })
		sink := &datagramSink{conn: sess, done: ctx.Done()}
		for ctx.Err() == nil && !sink.failed {
			_ = h.download.HandleDownload(ctx, parseBytes(query.Get("bytes")), sink)
		}
		return
	}
	lanes := laneOpener(func(ctx context.Context) (laneStream, error) {
		return sess.OpenUniStreamSync(ctx)
	})
	for range wtStreamCount(query) {
		wg.Go(func() { h.serveLane(ctx, lanes, query, live) })
	}
	// The session lasts as long as any lane is being served.
	wg.Wait()
}

type laneStream interface {
	io.WriteCloser
	CancelWrite(webtransport.StreamErrorCode)
	SetWriteDeadline(time.Time) error
}

type laneOpener func(context.Context) (laneStream, error)

func (h *wtDownload) serveLane(ctx context.Context, lanes laneOpener, query url.Values, live *sessionActivity) {
	for ctx.Err() == nil {
		str, err := lanes(ctx)
		if err != nil {
			return
		}
		lane := &laneWriter{w: str, live: live}
		withWTWriteStream(ctx, str, func() {
			_ = h.download.HandleDownload(ctx, parseBytes(query.Get("bytes")), lane)
		})
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

type wtUpload struct {
	upload    UploadHandler
	progress  *UploadProgress
	trusted   []netip.Prefix
	idleBound time.Duration
}

// NewWTUpload drains client-opened streams as upload lanes and serves the progress feed on one server-opened stream.
func NewWTUpload(upload UploadHandler, progress *UploadProgress, trusted []netip.Prefix, idleBound time.Duration) WTHandler {
	return &wtUpload{upload: upload, progress: progress, trusted: trusted, idleBound: idleBound}
}

func (h *wtUpload) HandleSession(ctx context.Context, sess *webtransport.Session, r *http.Request) {
	query := r.URL.Query()
	id := query.Get("id")
	// A stream carries no request, so its CONNECT identifies the upload owner.
	owner := UploadOwner(r, h.trusted)
	agg, access := h.progress.store.watchFor(id, owner)
	if access != uploadAccessOK {
		// The refusal is the whole answer, so the session and its admission slot end with it.
		h.serveRefusal(ctx, sess, access)
		lingerForPeer(ctx, sess, wtRefusalLinger)
		return
	}
	// The progress feed is server-generated, so its heartbeat must not count as activity.
	ctx, live := watchSession(ctx, h.idleBound)
	// Every goroutine below ends with the session's context, and the session ends only once they have.
	var wg sync.WaitGroup
	defer wg.Wait()
	defer live.cancel()
	wg.Go(func() { h.serveProgress(ctx, sess, agg) })
	if wtDatagramMode(query) {
		wg.Go(func() { h.drainDatagrams(ctx, sess, agg, id, owner, live) })
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
				h.serveLane(ctx, sess, str, id, owner, live)
			})
		default:
			str.CancelRead(0)
		}
	}
}

// wtRefusalLinger lets a peer read a refusal record before the session closes under it.
const wtRefusalLinger = 2 * time.Second

func lingerForPeer(ctx context.Context, sess *webtransport.Session, bound time.Duration) {
	timer := time.NewTimer(bound)
	defer timer.Stop()
	select {
	case <-ctx.Done():
	case <-sess.Context().Done():
	case <-timer.C:
	}
}

func (h *wtUpload) serveLane(ctx context.Context, sess *webtransport.Session, str *webtransport.ReceiveStream, id, owner string, live *sessionActivity) {
	// A blocked read watches neither the session's end nor its idle bound.
	defer transport.UnblockReadsOnDone(ctx, str)()
	src := idleTimeoutReader{str: str, timeout: uploadReadTimeout, live: live}
	_, err := h.upload.HandleUpload(ctx, id, owner, src)
	if refusal, ok := errors.AsType[*uploadRefusalError](err); ok {
		// Stream uploads have no response headers.
		h.serveRefusal(ctx, sess, refusal.access)
	}
	// Whatever ended the lane — a refusal, the idle bound, or a clean end — the stream is reset.
	str.CancelRead(0)
}

func (h *wtUpload) serveRefusal(ctx context.Context, sess *webtransport.Session, access uploadAccess) {
	str, err := sess.OpenUniStreamSync(ctx)
	if err != nil {
		return
	}
	withWTWriteStream(ctx, str, func() { writeRefusalRecord(str, access) })
}

func (h *wtUpload) drainDatagrams(ctx context.Context, sess *webtransport.Session, agg *uploadAgg, id, owner string, live *sessionActivity) {
	ctx, cancel := context.WithCancel(ctx)
	defer cancel()
	go func() {
		select {
		case <-agg.finished:
			cancel()
		case <-ctx.Done():
		}
	}()
	src := newIdleTimeoutSource(ctx, sess, uploadReadTimeout, live)
	_, _ = h.upload.HandleUpload(ctx, id, owner, src)
}

func (h *wtUpload) serveProgress(ctx context.Context, sess *webtransport.Session, agg *uploadAgg) {
	str, err := sess.OpenUniStreamSync(ctx)
	if err != nil {
		return
	}
	withWTWriteStream(ctx, str, func() { streamProgress(ctx, agg, str) })
}

type idleTimeoutReader struct {
	str     deadlineReader
	timeout time.Duration
	live    *sessionActivity
}

type deadlineReader interface {
	io.Reader
	SetReadDeadline(time.Time) error
}

func (r idleTimeoutReader) Read(p []byte) (int, error) {
	_ = r.str.SetReadDeadline(time.Now().Add(r.timeout))
	n, err := r.str.Read(p)
	if n > 0 {
		r.live.bump()
	}
	return n, err
}

type idleTimeoutSource struct {
	src     datagramSource
	ctx     context.Context
	cancel  context.CancelFunc
	timer   *time.Timer
	timeout time.Duration
	live    *sessionActivity
}

func newIdleTimeoutSource(parent context.Context, conn transport.DatagramConn, timeout time.Duration, live *sessionActivity) *idleTimeoutSource {
	ctx, cancel := context.WithCancel(parent)
	s := &idleTimeoutSource{ctx: ctx, cancel: cancel, live: live}
	s.src = datagramSource{conn: conn, ctx: ctx}
	s.timer = time.AfterFunc(timeout, cancel)
	s.timeout = timeout
	context.AfterFunc(ctx, func() { s.timer.Stop() })
	return s
}

func (s *idleTimeoutSource) Read(p []byte) (int, error) {
	s.timer.Reset(s.timeout)
	n, err := s.src.Read(p)
	if err != nil {
		s.timer.Stop()
		s.cancel()
	}
	if n > 0 {
		s.live.bump()
	}
	return n, err
}

func bumpOnPeerDatagrams(ctx context.Context, conn transport.DatagramConn, live *sessionActivity) {
	for {
		if _, err := conn.ReceiveDatagram(ctx); err != nil {
			return
		}
		live.bump()
	}
}

type datagramSink struct {
	conn   transport.DatagramConn
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
	conn transport.DatagramConn
	ctx  context.Context
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
	return copy(p, data), nil
}
