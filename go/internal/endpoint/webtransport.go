package endpoint

import (
	"context"
	"errors"
	"io"
	"net/http"
	"net/netip"
	"net/url"
	"strconv"
	"time"

	"github.com/quic-go/webtransport-go"
	"github.com/zR-JB/graphite-meter/go/internal/transport"
	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

// A WebTransport session hosts many logical requests, so each accepted stream
// and the datagram channel is dispatched into the same endpoints HTTP serves.
// The session URL carries what a query string would: /wt/upload?id=&datagrams=.

// WTHandler serves one accepted WebTransport session until it ends.
type WTHandler interface {
	HandleSession(ctx context.Context, sess *webtransport.Session, r *http.Request)
}

// wtDatagramPayload keeps a flooded datagram inside QUICInitialPacketSize once
// QUIC, HTTP/3 and WebTransport framing is accounted for.
const wtDatagramPayload = 1000

type wtPing struct{ ping Endpoint }

// NewWTPing serves the latency bus over session datagrams, where a ping that
// never returns is real packet loss.
func NewWTPing(ping Endpoint) WTHandler { return &wtPing{ping: ping} }

func (h *wtPing) HandleSession(ctx context.Context, sess *webtransport.Session, r *http.Request) {
	_ = h.ping.Handle(transport.NewWebTransportBusSession(ctx, sess, r.URL.Query()))
}

type wtDownload struct{ download Endpoint }

// NewWTDownload serves download lanes as bidirectional streams, each opened
// with a SIZE preamble, or as a datagram flood when SIZE arrives as a datagram.
func NewWTDownload(download Endpoint) WTHandler { return &wtDownload{download: download} }

func (h *wtDownload) HandleSession(ctx context.Context, sess *webtransport.Session, _ *http.Request) {
	go h.serveDatagrams(ctx, sess)
	for {
		str, err := sess.AcceptStream(ctx)
		if err != nil {
			return
		}
		go h.serveStream(ctx, str)
	}
}

// serveStream answers one lane's SIZE preamble on that same stream. A bad
// preamble closes the stream and leaves the session up.
func (h *wtDownload) serveStream(ctx context.Context, str *webtransport.Stream) {
	defer str.Close()
	f, err := wire.ReadStreamPreamble(str)
	if err != nil {
		var de *wire.DecodeError
		if errors.As(err, &de) {
			_, _ = io.WriteString(str, wire.EncodeStreamPreamble(wire.Frame{Op: wire.OpERR, Code: de.Code, Text: de.Text}))
		}
		return
	}
	if f.Op != wire.OpSIZE {
		_, _ = io.WriteString(str, wire.EncodeStreamPreamble(wire.Frame{Op: wire.OpERR, Code: wire.ErrBadOp, Text: f.Op}))
		return
	}
	_ = h.download.Handle(transport.NewWebTransportStreamSession(ctx, sizeQuery(f.Bytes), str, nil, ""))
}

func (h *wtDownload) serveDatagrams(ctx context.Context, sess *webtransport.Session) {
	for {
		data, err := sess.ReceiveDatagram(ctx)
		if err != nil {
			return
		}
		f, derr := wire.Decode(string(data))
		if derr != nil || f.Op != wire.OpSIZE {
			continue
		}
		sink := datagramSink{sess: sess}
		go h.download.Handle(transport.NewWebTransportStreamSession(ctx, sizeQuery(f.Bytes), sink, nil, "")) //nolint:errcheck // the sink reports client loss, not a server error
	}
}

type wtUpload struct {
	upload   Endpoint
	progress *UploadProgress
	trusted  []netip.Prefix
}

// NewWTUpload drains upload lanes opened as unidirectional streams and serves
// the progress feed on a bidirectional stream of the same session.
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
		go h.upload.Handle(transport.NewWebTransportStreamSession(ctx, query, nil, datagramSource{sess: sess, ctx: ctx}, owner)) //nolint:errcheck // a closed session ends the drain
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
	_ = str.SetReadDeadline(time.Now().Add(uploadReadTimeout))
	_ = h.upload.Handle(transport.NewWebTransportStreamSession(ctx, query, nil, str, owner))
}

// serveProgress runs the NDJSON feed on each bidirectional stream the client
// opens with a HI preamble; a second concurrent one is refused by the feed
// itself. A QUIC stream reaches the peer on its first write, so the preamble is
// what announces the stream at all.
func (h *wtUpload) serveProgress(ctx context.Context, sess *webtransport.Session, id, owner string) {
	for {
		str, err := sess.AcceptStream(ctx)
		if err != nil {
			return
		}
		go func() {
			defer str.Close()
			if f, err := wire.ReadStreamPreamble(str); err != nil || f.Op != wire.OpHI {
				_, _ = io.WriteString(str, wire.EncodeStreamPreamble(wire.Frame{Op: wire.OpERR, Code: wire.ErrBadOp, Text: "progress preamble"}))
				return
			}
			h.progress.HandleStream(ctx, id, owner, str)
		}()
	}
}

func sizeQuery(bytes uint64) url.Values {
	return url.Values{"bytes": {strconv.FormatUint(bytes, 10)}}
}

// datagramSink splits a download into unreliable datagrams. What arrives is
// goodput; what does not is loss the client sees directly.
type datagramSink struct{ sess *webtransport.Session }

func (s datagramSink) Write(p []byte) (int, error) {
	for off := 0; off < len(p); off += wtDatagramPayload {
		if err := s.sess.SendDatagram(p[off:min(off+wtDatagramPayload, len(p))]); err != nil {
			return off, err
		}
	}
	return len(p), nil
}

// datagramSource reads received datagrams as the byte stream the upload counter
// drains.
type datagramSource struct {
	sess *webtransport.Session
	ctx  context.Context
}

func (s datagramSource) Read(p []byte) (int, error) {
	data, err := s.sess.ReceiveDatagram(s.ctx)
	if err != nil {
		return 0, err
	}
	return copy(p, data), nil
}
