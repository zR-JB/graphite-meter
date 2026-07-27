package endpoint

import (
	"context"
	"io"
	"net/http"
	"net/netip"
	"net/url"
	"strconv"
	"sync"
	"sync/atomic"
	"time"

	"github.com/quic-go/webtransport-go"
	"github.com/zR-JB/graphite-meter/go/internal/transport"
	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

// A WebTransport session hosts many logical requests, and a stream carries no
// metadata of its own, so every parameter rides the session's CONNECT URL:
// /wt/download?bytes=&streams=&datagrams= and /wt/upload?id=&datagrams=.
// The server opens the download lanes and the upload progress feed, so no
// stream needs an opening frame to announce itself.

// WTHandler serves one accepted WebTransport session until it ends.
type WTHandler interface {
	HandleSession(ctx context.Context, sess *webtransport.Session, r *http.Request)
}

// wtDatagramPayload keeps a flooded datagram inside QUICInitialPacketSize once
// QUIC, HTTP/3 and WebTransport framing is accounted for.
const wtDatagramPayload = 1000

// wtVerifyLinger bounds an establish-only (bytes=0) session. A client closes one
// as soon as the handshake proves the path, so this only limits how long a
// parked one occupies a measurement slot.
const wtVerifyLinger = 30 * time.Second

// wtIdleBound closes a transfer session that carries nothing. A real one always
// has bytes moving, so silence means the peer is gone without saying so. The
// value is wire.WTIdleBound, the published contract both sides hold to; it is
// held atomically here because session handlers read it on goroutines a test's
// override races.
var wtIdleBound atomic.Int64

func init() { wtIdleBound.Store(int64(wire.WTIdleBound)) }

// SetWTIdleBoundForTest shortens the idle bound so a test need not wait it out.
func SetWTIdleBoundForTest(d time.Duration) func() {
	previous := wtIdleBound.Swap(int64(d))
	return func() { wtIdleBound.Store(previous) }
}

// sessionActivity ends a session once nothing has moved for wtIdleBound. Byte
// paths bump a counter and a watchdog samples it, so no timer is armed per byte.
type sessionActivity struct {
	n      atomic.Uint64
	cancel context.CancelFunc
}

// watchSession bounds a session by inactivity rather than by the peer closing
// it, so an abandoned one frees its admission slot without the client's help.
func watchSession(parent context.Context, bound time.Duration) (context.Context, *sessionActivity) {
	ctx, cancel := context.WithCancel(parent)
	a := &sessionActivity{cancel: cancel}
	go a.watch(ctx, bound)
	return ctx, a
}

// bump tolerates a nil receiver: a byte path outside a session has no watchdog.
func (a *sessionActivity) bump() {
	if a != nil {
		a.n.Add(1)
	}
}

func (a *sessionActivity) watch(ctx context.Context, bound time.Duration) {
	tick := time.NewTicker(bound / 2)
	defer tick.Stop()
	last := a.n.Load()
	quiet := 0
	for {
		select {
		case <-ctx.Done():
			return
		case <-tick.C:
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

type wtPing struct{ ping Endpoint }

// NewWTPing serves the latency bus over session datagrams, where a ping that
// never returns is real packet loss.
func NewWTPing(ping Endpoint) WTHandler { return &wtPing{ping: ping} }

// HandleSession runs the echo loop under the same idle watchdog the transfer
// sessions use. The bus stays on the request bound and the request bucket -- it
// is not a session route -- but an idle one would still hold its slot for the
// whole bound: QUIC's MaxIdleTimeout does not fire under keepalives. A bus is
// reaped only once the peer has genuinely stopped sending: a measuring client
// pings far inside the bound either way (80/250/600 ms in the browser, the
// native client's PingInterval, which is validated against the bound), a
// visible idle browser keepalives at 1 s, and a hidden one closes its bus
// rather than going quiet.
func (h *wtPing) HandleSession(ctx context.Context, sess *webtransport.Session, r *http.Request) {
	ctx, live := watchSession(ctx, time.Duration(wtIdleBound.Load()))
	bus := transport.NewWebTransportBusSession(ctx, liveDatagramConn{conn: sess, live: live}, r.URL.Query())
	_ = h.ping.Handle(bus)
}

// liveDatagramConn keeps a bus session alive on what the peer sends. Only the
// receive side counts: the echo this server writes back is its own traffic, so
// an idle bus is one the peer has stopped using.
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

type wtDownload struct{ download Endpoint }

// NewWTDownload serves ?bytes= per lane on ?streams= server-opened streams,
// each replaced when exhausted, or as a datagram flood repeated while the
// session lives when ?datagrams= is set. bytes=0 establishes a session that
// serves nothing, the transport check.
func NewWTDownload(download Endpoint) WTHandler { return &wtDownload{download: download} }

func (h *wtDownload) HandleSession(ctx context.Context, sess *webtransport.Session, r *http.Request) {
	query := r.URL.Query()
	// Parse rather than compare spellings: any zero request serves nothing, and
	// a lane loop over a zero length would spin without moving bytes. An
	// establish-only session holds an admission slot while it lives, and its
	// answer is the handshake, so it is bounded far below the session lifetime.
	if parseBytes(query.Get("bytes")) == 0 {
		linger := time.NewTimer(wtVerifyLinger)
		defer linger.Stop()
		select {
		case <-ctx.Done():
		case <-linger.C:
		}
		return
	}
	ctx, live := watchSession(ctx, time.Duration(wtIdleBound.Load()))
	if query.Get("datagrams") != "" {
		sink := &datagramSink{conn: sess, live: live}
		for ctx.Err() == nil && !sink.failed {
			_ = h.download.Handle(transport.NewWebTransportStreamSession(ctx, query, sink, nil, ""))
		}
		return
	}
	var wg sync.WaitGroup
	for range wtStreamCount(query) {
		wg.Go(func() { h.serveLane(ctx, sess, query, live) })
	}
	wg.Wait()
}

// serveLane keeps one lane filled: open a stream, write the requested bytes,
// close, replace, until the session ends. A lane that carried nothing means the
// peer is resetting what it is handed, so the loop stops rather than reopening
// streams at handshake speed for as long as the peer keeps refusing them.
func (h *wtDownload) serveLane(ctx context.Context, sess *webtransport.Session, query url.Values, live *sessionActivity) {
	for ctx.Err() == nil {
		str, err := sess.OpenUniStreamSync(ctx)
		if err != nil {
			return
		}
		lane := &laneWriter{w: str, live: live}
		stopOnCancel := transport.UnblockWritesOnDone(ctx, str)
		_ = h.download.Handle(transport.NewWebTransportStreamSession(ctx, query, lane, nil, ""))
		stopOnCancel()
		_ = str.Close()
		if !lane.moved {
			return
		}
	}
}

// laneWriter records whether a lane carried any bytes, and keeps the session
// alive while it does. The byte total is not wanted here, only that one write
// landed: a lane that carried nothing is a peer refusing what it is handed.
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

func wtStreamCount(query url.Values) int {
	n, err := strconv.Atoi(query.Get("streams"))
	if err != nil || n < 1 {
		return 1
	}
	return min(n, wire.WTMaxStreams)
}

type wtUpload struct {
	upload   Endpoint
	progress *UploadProgress
	trusted  []netip.Prefix
}

// NewWTUpload drains client-opened streams as upload lanes and serves the
// progress feed on one server-opened stream from session establishment.
func NewWTUpload(upload Endpoint, progress *UploadProgress, trusted []netip.Prefix) WTHandler {
	return &wtUpload{upload: upload, progress: progress, trusted: trusted}
}

func (h *wtUpload) HandleSession(ctx context.Context, sess *webtransport.Session, r *http.Request) {
	query := r.URL.Query()
	// A stream carries no request, so the session's CONNECT names the client the
	// upload id was minted for.
	owner := ClientKey(r, h.trusted)
	// The progress feed is server-generated, so its heartbeat must not count as
	// activity: only bytes the peer sent keep the session alive.
	ctx, live := watchSession(ctx, time.Duration(wtIdleBound.Load()))
	go h.serveProgress(ctx, sess, query.Get("id"), owner)
	if query.Get("datagrams") != "" {
		go h.drainDatagrams(ctx, sess, query, owner, live)
	}
	// The client opens these, so the ceiling the download side applies to its
	// own lanes applies here too: past it a lane is refused rather than served,
	// since each one holds a goroutine and a scratch buffer for the session.
	lanes := make(chan struct{}, wire.WTMaxStreams)
	for {
		str, err := sess.AcceptUniStream(ctx)
		if err != nil {
			return
		}
		select {
		case lanes <- struct{}{}:
			go func() {
				defer func() { <-lanes }()
				h.serveLane(ctx, str, query, owner, live)
			}()
		default:
			str.CancelRead(0)
		}
	}
}

func (h *wtUpload) serveLane(ctx context.Context, str *webtransport.ReceiveStream, query url.Values, owner string, live *sessionActivity) {
	src := idleTimeoutReader{str: str, timeout: uploadReadTimeout, live: live}
	_ = h.upload.Handle(transport.NewWebTransportStreamSession(ctx, query, nil, src, owner))
	// Whatever ended the lane — a refusal, the idle bound, or a clean end — the
	// stream is reset, so bytes this server will never read stop holding the
	// session's flow control.
	str.CancelRead(0)
}

// drainDatagrams counts received datagrams as upload bytes. A datagram carries
// no end marker, so the finalizing DELETE is what ends the drain: waiting out
// the idle bound instead would hold the terminal count back long past the
// client's own grace for it.
func (h *wtUpload) drainDatagrams(ctx context.Context, sess *webtransport.Session, query url.Values, owner string, live *sessionActivity) {
	agg, access := h.progress.store.getOrCreateForActivity(query.Get("id"), owner, false)
	if access != uploadAccessOK {
		return
	}
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
	_ = h.upload.Handle(transport.NewWebTransportStreamSession(ctx, query, nil, src, owner))
}

func (h *wtUpload) serveProgress(ctx context.Context, sess *webtransport.Session, id, owner string) {
	str, err := sess.OpenUniStreamSync(ctx)
	if err != nil {
		return
	}
	defer str.Close()
	defer transport.UnblockWritesOnDone(ctx, str)()
	h.progress.HandleStream(ctx, id, owner, str)
}

// idleTimeoutReader re-arms the stream's read deadline before every Read, so a
// lane is bounded by inactivity rather than one absolute deadline, matching the
// fetch path's per-POST bound.
type idleTimeoutReader struct {
	str     *webtransport.ReceiveStream
	timeout time.Duration
	live    *sessionActivity
}

func (r idleTimeoutReader) Read(p []byte) (int, error) {
	_ = r.str.SetReadDeadline(time.Now().Add(r.timeout))
	n, err := r.str.Read(p)
	if n > 0 {
		r.live.bump()
	}
	return n, err
}

// idleTimeoutSource is idleTimeoutReader for the datagram drain, whose receive
// blocks on the session context alone. One timer is re-armed per read rather
// than a context built per datagram: this is the drain's hot path.
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
	// Read is not the only way a drain ends: one refused before its first read
	// never reaches Read, and its timer would stay armed for the whole bound.
	// The source's context ends however the drain does, so disarm from there.
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

// datagramSink splits a download into unreliable datagrams. What arrives is
// goodput; what does not is loss the client sees directly. SendDatagram blocks
// on quic-go's bounded send queue, so the flood stays congestion-paced. failed
// latches a send error, ending the flood loop without a context round trip.
type datagramSink struct {
	conn   transport.DatagramConn
	live   *sessionActivity
	failed bool
}

func (s *datagramSink) Write(p []byte) (int, error) {
	// SendDatagram blocks on a full send queue and watches no ctx: a peer that
	// goes silent releases it at QUIC's MaxIdleTimeout (server.h3QUICConfig).
	for off := 0; off < len(p); off += wtDatagramPayload {
		if err := s.conn.SendDatagram(p[off:min(off+wtDatagramPayload, len(p))]); err != nil {
			s.failed = true
			return off, err
		}
	}
	s.live.bump()
	return len(p), nil
}

// datagramSource reads received datagrams as the byte stream the upload counter
// drains.
type datagramSource struct {
	conn transport.DatagramConn
	ctx  context.Context
}

// A datagram is delivered whole or not at all: silently dropping its tail would
// under-report the upload counter, which is the one number this drain feeds.
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
