package endpoint

import (
	"context"
	"net/http"
	"net/url"

	"github.com/coder/websocket"
	"github.com/zR-JB/graphite-meter/go/internal/auth"
	"github.com/zR-JB/graphite-meter/go/internal/transport"
)

// Registry maps paths to endpoints. The HTTP mux is built by walking it, and bus
// endpoints (WebSocket) resolve from the same registry. Adding an endpoint is one
// Register call, no listener code changes. WebTransport has no dispatcher
// (docs/ARCHITECTURE.md#roadmap).
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
// HTTP/1.1 Upgrade on the existing h1 origin, no new listener.
func (r *Registry) RegisterWS(path string, e Endpoint) {
	r.wsEndpoints[path] = e
}

// Mount attaches every registered endpoint onto mux: HTTP request/response
// handlers and WebSocket bus upgrades. parent bounds every bus's lifetime.
// Passing the server's run context lets srv.Shutdown unblock handlers parked in
// conn.Read/Write instead of hanging the shutdown window.
func (r *Registry) Mount(parent context.Context, mux *http.ServeMux) {
	r.MountWithOrigin(parent, mux, "")
}

// MountWithOrigin restricts browser cross-origin measurement access to one
// canonical authenticated UI origin. An empty origin preserves public mode.
func (r *Registry) MountWithOrigin(parent context.Context, mux *http.ServeMux, origin string) {
	for path, e := range r.httpEndpoints {
		mux.Handle(path, httpAdapterWithOrigin(e, origin))
	}
	for path, e := range r.wsEndpoints {
		mux.Handle(path, wsAdapterWithOrigin(parent, e, origin))
	}
}

// wsAdapter is the public-mode wsAdapterWithOrigin: no origin restriction.
func wsAdapter(parent context.Context, e Endpoint) http.Handler {
	return wsAdapterWithOrigin(parent, e, "")
}

// wsAdapterWithOrigin upgrades the request to a WebSocket and runs the endpoint
// against a websocketSession exposing the message bus. A non-empty allowedOrigin
// means the server holds session state, so the upgrade is pinned to that one UI
// origin. An empty allowedOrigin is public mode.
func wsAdapterWithOrigin(parent context.Context, e Endpoint, allowedOrigin string) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if allowedOrigin != "" && r.Header.Get("Origin") != "" && r.Header.Get("Origin") != allowedOrigin {
			http.Error(w, "forbidden", http.StatusForbidden)
			return
		}
		conn, err := websocket.Accept(w, r, &websocket.AcceptOptions{
			// Public mode is auth-less and cookie-less, holding no session state a
			// forged origin could abuse. It mirrors the wildcard CORS on HTTP.
			InsecureSkipVerify: allowedOrigin == "",
			OriginPatterns: func() []string {
				if allowedOrigin == "" {
					return nil
				}
				// allowedOrigin is the String() of a *url.URL the auth service
				// already parsed at startup, so re-parsing it cannot fail.
				u, _ := url.Parse(allowedOrigin)
				return []string{u.Host}
			}(),
			CompressionMode: websocket.CompressionDisabled,
		})
		if err != nil {
			return // Accept already wrote the handshake-failure response
		}
		// Bus frames are tiny text control messages (opcodes.go). A read limit well
		// under the library default stops a peer forcing a large buffered allocation.
		conn.SetReadLimit(2048)
		// Accept hijacks the conn, which makes r.Context() unreliable (see its
		// docs). parent keeps the bus bounded by the server's shutdown instead.
		var ctx context.Context
		var cancel context.CancelFunc
		if deadline, ok := r.Context().Deadline(); ok {
			ctx, cancel = context.WithDeadline(parent, deadline)
		} else {
			ctx, cancel = context.WithCancel(parent)
		}
		if allowedOrigin != "" {
			stopRequest := context.AfterFunc(r.Context(), cancel)
			defer stopRequest()
		}
		defer cancel()
		defer conn.CloseNow()

		s := transport.NewWebSocketSession(ctx, conn, r.URL.Query())
		err = e.Handle(s)
		if allowedOrigin != "" && auth.SessionEnded(r.Context()) {
			conn.Close(websocket.StatusPolicyViolation, "authentication required")
			return
		}
		if err != nil {
			conn.Close(websocket.StatusInternalError, "handler error")
			return
		}
		conn.Close(websocket.StatusNormalClosure, "")
	})
}

// httpAdapter is the public-mode httpAdapterWithOrigin: wildcard CORS.
func httpAdapter(e Endpoint) http.Handler { return httpAdapterWithOrigin(e, "") }

// httpAdapterWithOrigin wraps an Endpoint as an http.Handler: it applies the
// global CORS + timing headers, short-circuits CORS preflight OPTIONS, and runs
// the endpoint against an httpSession.
func httpAdapterWithOrigin(e Endpoint, origin string) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		setCommonHeaders(w, origin)
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

// setCommonHeaders admits cross-origin measurement: the UI and the measurement
// listener sit on different ports. Timing-Allow-Origin gates Resource Timing.
// An empty origin is public mode and answers with wildcards. A named origin
// narrows every header to it and admits credentials, which a wildcard may not.
func setCommonHeaders(w http.ResponseWriter, origin string) {
	h := w.Header()
	if origin != "" {
		h.Set("Access-Control-Allow-Origin", origin)
		h.Set("Access-Control-Allow-Credentials", "true")
		h.Set("Access-Control-Allow-Methods", "GET, POST, DELETE, OPTIONS")
		h.Set("Access-Control-Allow-Headers", "Authorization, Content-Type, X-CSRF-Token")
		h.Set("Access-Control-Expose-Headers", "Graphite-Meter-Auth, Graphite-Meter-Auth-URL")
		h.Set("Timing-Allow-Origin", origin)
		h.Add("Vary", "Origin")
		return
	}
	h.Set("Access-Control-Allow-Origin", "*")
	h.Set("Access-Control-Allow-Methods", "GET, POST, DELETE, OPTIONS")
	h.Set("Access-Control-Allow-Headers", "*")
	h.Set("Timing-Allow-Origin", "*")
}
