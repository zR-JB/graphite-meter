package endpoint

import (
	"context"
	"net/http"
	"net/netip"
	"net/url"
	"strconv"
	"sync"
	"time"

	"github.com/quic-go/webtransport-go"
	"github.com/zR-JB/graphite-meter/go/internal/transport"
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

// wtMaxStreams caps the download lanes one session may request.
const wtMaxStreams = 16

// wtDatagramPayload keeps a flooded datagram inside QUICInitialPacketSize once
// QUIC, HTTP/3 and WebTransport framing is accounted for.
const wtDatagramPayload = 1000

// wtVerifyLinger bounds an establish-only (bytes=0) session. A client closes one
// as soon as the handshake proves the path, so this only limits how long a
// parked one occupies a measurement slot.
const wtVerifyLinger = 30 * time.Second

type wtPing struct{ ping Endpoint }

// NewWTPing serves the latency bus over session datagrams, where a ping that
// never returns is real packet loss.
func NewWTPing(ping Endpoint) WTHandler { return &wtPing{ping: ping} }

func (h *wtPing) HandleSession(ctx context.Context, sess *webtransport.Session, r *http.Request) {
	_ = h.ping.Handle(transport.NewWebTransportBusSession(ctx, sess, r.URL.Query()))
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
	if query.Get("datagrams") != "" {
		sink := &datagramSink{conn: sess}
		for ctx.Err() == nil && !sink.failed {
			_ = h.download.Handle(transport.NewWebTransportStreamSession(ctx, query, sink, nil, ""))
		}
		return
	}
	var wg sync.WaitGroup
	for range wtStreamCount(query) {
		wg.Go(func() { h.serveLane(ctx, sess, query) })
	}
	wg.Wait()
}

// serveLane keeps one lane filled: open a stream, write the requested bytes,
// close, replace, until the session ends.
func (h *wtDownload) serveLane(ctx context.Context, sess *webtransport.Session, query url.Values) {
	for ctx.Err() == nil {
		str, err := sess.OpenUniStreamSync(ctx)
		if err != nil {
			return
		}
		// A blocked write observes neither the context nor a dead session on its
		// own; the deadline poke unblocks a wait for a close that never arrives.
		stopOnCancel := context.AfterFunc(ctx, func() {
			str.CancelWrite(0)
			_ = str.SetWriteDeadline(time.Now())
		})
		_ = h.download.Handle(transport.NewWebTransportStreamSession(ctx, query, str, nil, ""))
		stopOnCancel()
		_ = str.Close()
	}
}

func wtStreamCount(query url.Values) int {
	n, err := strconv.Atoi(query.Get("streams"))
	if err != nil || n < 1 {
		return 1
	}
	return min(n, wtMaxStreams)
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
	go h.serveProgress(ctx, sess, query.Get("id"), owner)
	if query.Get("datagrams") != "" {
		// Bounded by inactivity like the stream lanes, so a silent session
		// cannot pin its drain and hold the aggregate off the sweeper.
		src := idleTimeoutSource{src: datagramSource{conn: sess, ctx: ctx}, timeout: uploadReadTimeout}
		go h.upload.Handle(transport.NewWebTransportStreamSession(ctx, query, nil, src, owner)) //nolint:errcheck // a closed session ends the drain
	}
	for {
		str, err := sess.AcceptUniStream(ctx)
		if err != nil {
			return
		}
		go h.serveLane(ctx, str, query, owner)
	}
}

func (h *wtUpload) serveLane(ctx context.Context, str *webtransport.ReceiveStream, query url.Values, owner string) {
	src := idleTimeoutReader{str: str, timeout: uploadReadTimeout}
	// A refused lane is told so: without the reset the client would sit on flow
	// control with its buffered bytes stranded until the session ends.
	if err := h.upload.Handle(transport.NewWebTransportStreamSession(ctx, query, nil, src, owner)); err != nil {
		str.CancelRead(0)
	}
}

func (h *wtUpload) serveProgress(ctx context.Context, sess *webtransport.Session, id, owner string) {
	str, err := sess.OpenUniStreamSync(ctx)
	if err != nil {
		return
	}
	defer str.Close()
	defer context.AfterFunc(ctx, func() {
		str.CancelWrite(0)
		_ = str.SetWriteDeadline(time.Now())
	})()
	h.progress.HandleStream(ctx, id, owner, str)
}

// idleTimeoutReader re-arms the stream's read deadline before every Read, so a
// lane is bounded by inactivity rather than one absolute deadline, matching the
// fetch path's per-POST bound.
type idleTimeoutReader struct {
	str     *webtransport.ReceiveStream
	timeout time.Duration
}

func (r idleTimeoutReader) Read(p []byte) (int, error) {
	_ = r.str.SetReadDeadline(time.Now().Add(r.timeout))
	return r.str.Read(p)
}

// idleTimeoutSource is idleTimeoutReader for the datagram drain, whose receive
// blocks on the session context alone.
type idleTimeoutSource struct {
	src     datagramSource
	timeout time.Duration
}

func (s idleTimeoutSource) Read(p []byte) (int, error) {
	ctx, cancel := context.WithTimeout(s.src.ctx, s.timeout)
	defer cancel()
	return datagramSource{conn: s.src.conn, ctx: ctx}.Read(p)
}

// datagramSink splits a download into unreliable datagrams. What arrives is
// goodput; what does not is loss the client sees directly. SendDatagram blocks
// on quic-go's bounded send queue, so the flood stays congestion-paced. failed
// latches a send error, ending the flood loop without a context round trip.
type datagramSink struct {
	conn   transport.DatagramConn
	failed bool
}

func (s *datagramSink) Write(p []byte) (int, error) {
	for off := 0; off < len(p); off += wtDatagramPayload {
		if err := s.conn.SendDatagram(p[off:min(off+wtDatagramPayload, len(p))]); err != nil {
			s.failed = true
			return off, err
		}
	}
	return len(p), nil
}

// datagramSource reads received datagrams as the byte stream the upload counter
// drains.
type datagramSource struct {
	conn transport.DatagramConn
	ctx  context.Context
}

func (s datagramSource) Read(p []byte) (int, error) {
	data, err := s.conn.ReceiveDatagram(s.ctx)
	if err != nil {
		return 0, err
	}
	return copy(p, data), nil
}
