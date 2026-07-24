package endpoint

import (
	"context"
	"net/http"
	"net/url"

	"github.com/coder/websocket"
	"github.com/zR-JB/graphite-meter/go/internal/auth"
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
// HTTP/1.1 Upgrade on the existing h1 origin — no new listener. The WebTransport
// dispatcher will resolve bus endpoints from this same registry by path.
func (r *Registry) RegisterWS(path string, e Endpoint) {
	r.wsEndpoints[path] = e
}

// Mount attaches all registered endpoints onto mux: HTTP request/response
// handlers and WebSocket bus upgrades. parent bounds every bus's lifetime — it is
// the server's run context, so srv.Shutdown cancels it and in-flight bus handlers
// (conn.Read/Write) return promptly instead of hanging the shutdown window.
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
// against a websocketSession exposing the message bus.
//
// An empty allowedOrigin is public mode: cross-origin upgrades are allowed
// (InsecureSkipVerify) to mirror the permissive Access-Control-Allow-Origin: *
// the HTTP endpoints already set — this is a public, auth-less, cookie-less
// measurement bus (app on :7246 measuring against :7248), so there is no session
// state for a forged origin to abuse. A non-empty allowedOrigin means the server
// does hold session state, so the upgrade is pinned to that one UI origin.
func wsAdapterWithOrigin(parent context.Context, e Endpoint, allowedOrigin string) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if allowedOrigin != "" && r.Header.Get("Origin") != "" && r.Header.Get("Origin") != allowedOrigin {
			http.Error(w, "forbidden", http.StatusForbidden)
			return
		}
		conn, err := websocket.Accept(w, r, &websocket.AcceptOptions{
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
		// Message-bus frames are tiny text control messages (opcodes.go); cap the
		// read well below the library default so a peer cannot force a large
		// buffered allocation per connection.
		conn.SetReadLimit(2048)
		// The conn is hijacked, so r.Context() is no longer reliable (see Accept
		// docs). Bound the bus with a context derived from the SERVER's run context
		// (not Background): cancelled when Handle returns AND on srv.Shutdown, so a
		// handler parked in conn.Read/Write unblocks at shutdown instead of hanging.
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

// setCommonHeaders lets the client measure cross-origin (app on :7246,
// measuring against :7248) while still reading accurate Resource Timing, which
// Timing-Allow-Origin gates. An empty origin is public mode and answers with
// wildcards; a named origin narrows every header to it and admits credentials,
// which a wildcard may never do.
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
