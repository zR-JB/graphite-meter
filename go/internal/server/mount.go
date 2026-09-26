package server

import (
	"context"
	"io"
	"net/http"
	"strings"

	"github.com/coder/websocket"
	"github.com/quic-go/webtransport-go"
	"github.com/zR-JB/graphite-meter/go/internal/auth"
	"github.com/zR-JB/graphite-meter/go/internal/endpoint"
	"github.com/zR-JB/graphite-meter/go/internal/route"
)

// muxTopology names the measurement surfaces one listener serves.
type muxTopology struct {
	spa, discovery, latency, transfers, bootstrap bool
	// requiredProto confines transfer routes to one HTTP major version; 0 accepts any.
	requiredProto int
	wt            *webtransport.Server
}

// mounter composes each route as admission, then the adapter for its kind, then the protocol check.
type mounter struct {
	ctx   context.Context // the server's lifetime; upgraded connections outlive their requests
	mux   *http.ServeMux
	e     *endpoints
	authn *auth.Service
}

func newMux(ctx context.Context, e *endpoints, topo muxTopology, spa http.Handler, authn *auth.Service) http.Handler {
	m := &mounter{ctx: ctx, mux: http.NewServeMux(), e: e, authn: authn}
	if topo.discovery {
		m.http(route.Preflight, http.HandlerFunc(e.discovery.ServePreflight), 0)
		m.http(route.Servers, http.HandlerFunc(e.discovery.ServeServers), 0)
	}
	if topo.bootstrap {
		m.http(route.Probe, e.bootstrapProbe, 0)
	} else {
		m.http(route.Probe, e.probe, 0)
	}
	if topo.transfers {
		proto := topo.requiredProto
		m.http(route.Download, e.download, proto)
		m.http(route.Upload, e.upload, proto)
		m.http(route.UploadSession, http.HandlerFunc(e.upload.ServeSession), proto)
		m.http(route.UploadCheckpoint, http.HandlerFunc(e.upload.ServeCheckpoint), proto)
		m.http(route.UploadProgress, http.HandlerFunc(e.upload.ServeProgress), proto)
		m.http(route.WTSession, authn.SocketTokenHandler(route.WebTransport), proto)
	}
	if topo.latency {
		m.http(route.WSSession, authn.SocketTokenHandler(route.WebSocket), 0)
		m.handle(route.Ping, m.webSocketPing())
	}
	if topo.wt != nil {
		m.handle(route.WTDownload, m.webTransport(topo.wt, endpoint.WTDownload(e.download)))
		m.handle(route.WTUpload, m.webTransport(topo.wt, endpoint.WTUpload(e.upload)))
		m.handle(route.WTPing, m.webTransport(topo.wt, endpoint.WTPing))
	}
	if topo.spa {
		authn.Mount(m.mux)
		m.mux.Handle("/", spa)
	}
	return rejectDotSegments(m.mux)
}

func (m *mounter) handle(path string, h http.Handler) {
	spec, ok := route.Lookup(path)
	if !ok {
		panic("server: mounting an unpublished route " + path)
	}
	if spec.Admission != route.Unmetered {
		h = m.e.admission.wrap(h, spec, m.e.trusted, m.authn)
	}
	for method := range spec.Methods() {
		m.mux.Handle(method+" "+path, h)
	}
}

func (m *mounter) http(path string, h http.Handler, proto int) {
	m.handle(path, http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		m.authn.MeasurementCORS(w.Header(), r)
		if proto != 0 && r.ProtoMajor != proto {
			http.NotFound(w, r)
			return
		}
		h.ServeHTTP(w, r)
	}))
	m.mux.HandleFunc(http.MethodOptions+" "+path, m.authn.ServePreflight)
}

// wsPingReadLimit bounds a probe frame; a valid PING is at most 15 bytes.
const wsPingReadLimit = 2048

func (m *mounter) webSocketPing() http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		// Enforce binds the origin under authentication; public mode has no session state to abuse.
		conn, err := websocket.Accept(w, r, &websocket.AcceptOptions{
			InsecureSkipVerify: true,
			CompressionMode:    websocket.CompressionDisabled,
		})
		if err != nil {
			return // Accept already wrote the handshake-failure response
		}
		defer conn.CloseNow()
		conn.SetReadLimit(wsPingReadLimit)
		// Ending the bus is a close handshake, which also unblocks its reads and writes.
		ctx, cancel := linkedContext(m.ctx, r.Context())
		defer cancel()
		ctx, live := endpoint.WatchIdle(ctx, m.e.idleBound)
		end := func() {
			end := endpoint.EndOf(ctx, m.ctx)
			conn.Close(websocket.StatusCode(end.WS), end.Reason)
		}
		ended := context.AfterFunc(ctx, end)
		// The read limit admits one extra byte, so an oversized message fails before filling buf.
		var buf [wsPingReadLimit + 2]byte
		endpoint.ServePing(func() ([]byte, error) {
			_, message, err := conn.Reader(context.Background())
			if err != nil {
				return nil, err
			}
			live.Bump()
			n, err := io.ReadFull(message, buf[:])
			switch err {
			case io.ErrUnexpectedEOF, io.EOF:
				return buf[:n], nil
			case nil:
				return nil, io.ErrShortBuffer
			}
			return nil, err
		}, func(reply []byte) error { return conn.Write(context.Background(), websocket.MessageText, reply) })
		if ended() {
			end()
		}
	})
}

func (m *mounter) webTransport(server *webtransport.Server, serve endpoint.SessionHandler) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		sess, err := server.Upgrade(w, r)
		if err != nil {
			http.Error(w, "webtransport upgrade failed", http.StatusBadRequest)
			return
		}
		ctx, cancel := linkedContext(m.ctx, r.Context(), sess.Context())
		defer cancel()
		ctx, live := endpoint.WatchIdle(ctx, m.e.idleBound)
		defer func() {
			end := endpoint.EndOf(ctx, m.ctx)
			_ = sess.CloseWithError(webtransport.SessionErrorCode(end.WT), end.Reason)
		}()
		serve(ctx, sess, r, live)
	})
}

// linkedContext ends with parent or any of ends, keeping its cause; upgraded channels outlive their request.
func linkedContext(parent context.Context, ends ...context.Context) (context.Context, context.CancelFunc) {
	ctx, cancel := context.WithCancelCause(parent)
	stops := make([]func() bool, len(ends))
	for i, end := range ends {
		stops[i] = context.AfterFunc(end, func() { cancel(context.Cause(end)) })
	}
	return ctx, func() {
		for _, stop := range stops {
			stop()
		}
		cancel(nil)
	}
}

// rejectDotSegments refuses dot segments and backslashes before ServeMux can redirect them.
func rejectDotSegments(next http.Handler) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if strings.Contains(r.URL.Path, `\`) {
			http.NotFound(w, r)
			return
		}
		for segment := range strings.SplitSeq(r.URL.Path, "/") {
			if segment == "." || segment == ".." {
				http.NotFound(w, r)
				return
			}
		}
		next.ServeHTTP(w, r)
	})
}
