package endpoint

import (
	"context"
	"net/http"

	"github.com/coder/websocket"
	"github.com/zR-JB/graphite-meter/go/internal/transport"
)

// Registry maps paths to endpoints. The HTTP mux is built by walking it; bus
// endpoints (WebSocket today) resolve from the same registry. Adding an
// endpoint is a Register call — no listener code changes. WebTransport is
// reserved for Stage 5 — see docs/ARCHITECTURE.md#roadmap.
type Registry struct {
	httpEndpoints map[string]Endpoint
	wsEndpoints   map[string]Endpoint
}

// NewRegistry returns an empty registry.
func NewRegistry() *Registry {
	return &Registry{
		httpEndpoints: make(map[string]Endpoint),
		wsEndpoints:   make(map[string]Endpoint),
	}
}

// RegisterHTTP mounts an endpoint as an HTTP request/response handler at path.
func (r *Registry) RegisterHTTP(path string, e Endpoint) {
	r.httpEndpoints[path] = e
}

// RegisterWS mounts an endpoint as a WebSocket bus at path. The upgrade is an
// HTTP/1.1 Upgrade on the existing h1 origin — no new listener. Reserved for
// Stage 5 — see docs/ARCHITECTURE.md#roadmap: the WebTransport dispatcher will
// reuse this same registry to resolve bus endpoints by path.
func (r *Registry) RegisterWS(path string, e Endpoint) {
	r.wsEndpoints[path] = e
}

// Mount attaches all registered endpoints onto mux: HTTP request/response
// handlers and WebSocket bus upgrades. parent bounds every bus's lifetime — it is
// the server's run context, so srv.Shutdown cancels it and in-flight bus handlers
// (conn.Read/Write) return promptly instead of hanging the shutdown window.
func (r *Registry) Mount(parent context.Context, mux *http.ServeMux) {
	for path, e := range r.httpEndpoints {
		mux.Handle(path, httpAdapter(e))
	}
	for path, e := range r.wsEndpoints {
		mux.Handle(path, wsAdapter(parent, e))
	}
}

// wsAdapter upgrades the request to a WebSocket and runs the endpoint against a
// websocketSession exposing the message bus. Cross-origin upgrades are allowed
// (InsecureSkipVerify) to mirror the permissive Access-Control-Allow-Origin: *
// the HTTP endpoints already set — this is a public, auth-less, cookie-less
// measurement bus (app on :7246 measuring against :7248), so there is no session
// state for a forged origin to abuse.
func wsAdapter(parent context.Context, e Endpoint) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		conn, err := websocket.Accept(w, r, &websocket.AcceptOptions{
			InsecureSkipVerify: true,
			CompressionMode:    websocket.CompressionDisabled,
		})
		if err != nil {
			return // Accept already wrote the handshake-failure response
		}
		// The conn is hijacked, so r.Context() is no longer reliable (see Accept
		// docs). Bound the bus with a context derived from the SERVER's run context
		// (not Background): cancelled when Handle returns AND on srv.Shutdown, so a
		// handler parked in conn.Read/Write unblocks at shutdown instead of hanging.
		ctx, cancel := context.WithCancel(parent)
		defer cancel()
		defer conn.CloseNow()

		s := transport.NewWebSocketSession(ctx, conn, r.URL.Query())
		if err := e.Handle(s); err != nil {
			conn.Close(websocket.StatusInternalError, "handler error")
			return
		}
		conn.Close(websocket.StatusNormalClosure, "")
	})
}

// httpAdapter wraps an Endpoint as an http.Handler: it applies the global CORS
// + timing headers, short-circuits CORS preflight OPTIONS, and runs the
// endpoint against an httpSession.
func httpAdapter(e Endpoint) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		setCommonHeaders(w)
		if r.Method == http.MethodOptions {
			w.WriteHeader(http.StatusNoContent)
			return
		}
		s := transport.NewHTTPSession(w, r)
		if err := e.Handle(s); err != nil {
			http.Error(w, err.Error(), http.StatusInternalServerError)
		}
	})
}

// setCommonHeaders applies permissive CORS and Timing-Allow-Origin so the
// client can measure cross-origin (app on :7246, measuring against :7248) with
// accurate Resource Timing.
func setCommonHeaders(w http.ResponseWriter) {
	h := w.Header()
	h.Set("Access-Control-Allow-Origin", "*")
	h.Set("Access-Control-Allow-Methods", "GET, POST, DELETE, OPTIONS")
	h.Set("Access-Control-Allow-Headers", "*")
	h.Set("Timing-Allow-Origin", "*")
}
