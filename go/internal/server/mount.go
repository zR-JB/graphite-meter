package server

import (
	"context"
	"io"
	"net/http"
	"net/url"
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

// mounter composes every measurement route the same way: authentication has
// already run around the whole mux; then come admission, the transport adapter
// the route's kind selects, and the listener's protocol check.
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
		m.http(route.WTSession, endpoint.SocketToken(m.minter(authn.MintWebTransportSessionToken)), proto)
	}
	if topo.latency {
		m.http(route.WSSession, endpoint.SocketToken(m.minter(authn.MintWebSocketSessionToken)), 0)
		m.handle(route.Ping, m.webSocketPing())
	}
	if topo.wt != nil {
		m.handle(route.WTDownload, m.webTransport(topo.wt, endpoint.WTDownload(e.stream, e.wtIdleBound)))
		m.handle(route.WTUpload, m.webTransport(topo.wt, endpoint.WTUpload(e.upload, e.receive, e.wtIdleBound)))
		m.handle(route.WTPing, m.webTransport(topo.wt, endpoint.WTPing(e.wtIdleBound)))
	}
	if topo.spa {
		authn.Mount(m.mux)
		m.mux.Handle("/", spa)
	}
	return rejectDotSegments(m.mux)
}

// handle mounts h at path under one ServeMux pattern per method the route
// publishes, so any other method is a 405 that reaches no handler.
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

// http mounts a request-shaped measurement route behind its CORS answer and,
// when proto is set, the listener's protocol.
func (m *mounter) http(path string, h http.Handler, proto int) {
	m.handle(path, http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		m.authn.MeasurementCORS(w.Header(), r)
		if proto != 0 && r.ProtoMajor != proto {
			http.NotFound(w, r)
			return
		}
		h.ServeHTTP(w, r)
	}))
	// Authentication answers every measurement preflight itself; public mode answers here, unmetered.
	m.mux.HandleFunc(http.MethodOptions+" "+path, func(w http.ResponseWriter, r *http.Request) {
		m.authn.MeasurementCORS(w.Header(), r)
		w.WriteHeader(http.StatusNoContent)
	})
}

// minter is the socket-token mint under authentication and nil in public mode, whose tokens are empty.
func (m *mounter) minter(mint endpoint.SocketTokenMinter) endpoint.SocketTokenMinter {
	if m.authn.Enabled() {
		return mint
	}
	return nil
}

// wsPingReadLimit bounds a probe frame; a valid PING is at most 15 bytes.
const wsPingReadLimit = 2048

// webSocketPing upgrades to the WebSocket latency bus and serves one probe per text frame.
func (m *mounter) webSocketPing() http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		allowed := m.authn.PublicOrigin()
		if approved := auth.BrowserOrigin(r); approved != "" {
			allowed = approved
		}
		var patterns []string
		if allowed != "" {
			if origin := r.Header.Get("Origin"); origin != "" && origin != allowed {
				http.Error(w, "forbidden", http.StatusForbidden)
				return
			}
			// allowed is a canonical origin the auth service parsed at startup or on grant approval.
			u, _ := url.Parse(allowed)
			patterns = []string{u.Host}
		}
		conn, err := websocket.Accept(w, r, &websocket.AcceptOptions{
			// Public mode is auth-less and cookie-less, holding no session state a forged origin could abuse.
			InsecureSkipVerify: allowed == "",
			OriginPatterns:     patterns,
			CompressionMode:    websocket.CompressionDisabled,
		})
		if err != nil {
			return // Accept already wrote the handshake-failure response
		}
		defer conn.CloseNow()
		conn.SetReadLimit(wsPingReadLimit)
		// The bus ends with its lifetime, the server or the login by a close
		// handshake, which also unblocks the probe loop. Its reads and writes
		// then need no context of their own.
		ctx, cancel := linkedContext(m.ctx, r.Context())
		defer cancel()
		end := func() {
			if auth.SessionEnded(r.Context()) {
				conn.Close(websocket.StatusPolicyViolation, "authentication required")
				return
			}
			conn.Close(websocket.StatusNormalClosure, "")
		}
		ended := context.AfterFunc(ctx, end)
		// The library reads one byte past its limit, so an oversized message fails before it can fill buf.
		var buf [wsPingReadLimit + 2]byte
		endpoint.ServePing(func() ([]byte, error) {
			_, message, err := conn.Reader(context.Background())
			if err != nil {
				return nil, err
			}
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

// webTransport upgrades a CONNECT to a session and serves it until either side ends it.
func (m *mounter) webTransport(server *webtransport.Server, serve endpoint.SessionHandler) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		sess, err := server.Upgrade(w, r)
		if err != nil {
			http.Error(w, "webtransport upgrade failed", http.StatusBadRequest)
			return
		}
		defer sess.CloseWithError(0, "") //nolint:errcheck // the session is going away either way
		ctx, cancel := linkedContext(m.ctx, r.Context(), sess.Context())
		defer cancel()
		serve(ctx, sess, r)
	})
}

// linkedContext ends when parent or any of ends does. An upgraded request's
// own context no longer tracks its connection, and server shutdown does not
// reach it, so upgraded channels are bounded by both.
func linkedContext(parent context.Context, ends ...context.Context) (context.Context, context.CancelFunc) {
	ctx, cancel := context.WithCancel(parent)
	stops := make([]func() bool, len(ends))
	for i, end := range ends {
		stops[i] = context.AfterFunc(end, cancel)
	}
	return ctx, func() {
		for _, stop := range stops {
			stop()
		}
		cancel()
	}
}

// rejectDotSegments refuses dot segments and backslashes before ServeMux canonicalization can redirect them anywhere.
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

// wtOriginCheck is the only origin policy a WebTransport CONNECT passes: under
// authentication a browser CONNECT must come from the canonical origin, or from
// the origin its browser grant approved. A CONNECT without Origin is a native client.
func wtOriginCheck(authn *auth.Service) func(*http.Request) bool {
	return func(r *http.Request) bool {
		if !authn.Enabled() {
			return true
		}
		origin := r.Header.Get("Origin")
		if approved := auth.BrowserOrigin(r); approved != "" {
			return origin == approved
		}
		return origin == "" || origin == authn.PublicOrigin()
	}
}
