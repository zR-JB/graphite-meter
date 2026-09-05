package endpoint

import (
	"context"
	"maps"
	"net/http"
	"net/url"

	"github.com/coder/websocket"
	"github.com/quic-go/webtransport-go"
	"github.com/zR-JB/graphite-meter/go/internal/auth"
	"github.com/zR-JB/graphite-meter/go/internal/cors"
	"github.com/zR-JB/graphite-meter/go/internal/transport"
)

// Registry maps paths to endpoints.
type Registry struct {
	httpEndpoints map[string]HTTPHandler
	wsEndpoints   map[string]MessageHandler
	wtEndpoints   map[string]WTHandler
}

// Kinds reports how each registered path is reached.
func (r *Registry) Kinds() map[string]string {
	kinds := make(map[string]string, len(r.httpEndpoints)+len(r.wsEndpoints)+len(r.wtEndpoints))
	for path := range maps.Keys(r.httpEndpoints) {
		kinds[path] = "http"
	}
	for path := range maps.Keys(r.wsEndpoints) {
		kinds[path] = "ws"
	}
	for path := range maps.Keys(r.wtEndpoints) {
		kinds[path] = "wt"
	}
	return kinds
}

// NewRegistry returns an empty registry.
func NewRegistry() *Registry {
	return &Registry{
		httpEndpoints: make(map[string]HTTPHandler),
		wsEndpoints:   make(map[string]MessageHandler),
		wtEndpoints:   make(map[string]WTHandler),
	}
}

// RegisterHTTP mounts an endpoint as an HTTP request/response handler at path.
func (r *Registry) RegisterHTTP(path string, e HTTPHandler) {
	r.httpEndpoints[path] = e
}

// RegisterWS mounts an endpoint as a WebSocket bus at path.
func (r *Registry) RegisterWS(path string, e MessageHandler) {
	r.wsEndpoints[path] = e
}

// RegisterWT mounts a WebTransport session handler at path.
func (r *Registry) RegisterWT(path string, h WTHandler) {
	r.wtEndpoints[path] = h
}

// MountWebTransport attaches registered session handlers onto mux.
func (r *Registry) MountWebTransport(parent context.Context, mux *http.ServeMux, server *webtransport.Server) {
	for path, h := range r.wtEndpoints {
		mux.Handle(path, wtAdapter(parent, h, server))
	}
}

func wtAdapter(parent context.Context, h WTHandler, server *webtransport.Server) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		sess, err := server.Upgrade(w, r)
		if err != nil {
			http.Error(w, "webtransport upgrade failed", http.StatusBadRequest)
			return
		}
		defer sess.CloseWithError(0, "") //nolint:errcheck // the session is going away either way
		ctx, cancel := context.WithCancel(parent)
		defer cancel()
		defer context.AfterFunc(r.Context(), cancel)()
		defer context.AfterFunc(sess.Context(), cancel)()
		h.HandleSession(ctx, sess, r)
	})
}

// Mount attaches every registered endpoint onto mux.
func (r *Registry) Mount(parent context.Context, mux *http.ServeMux) {
	r.MountWithOrigin(parent, mux, "")
}

// MountWithOrigin restricts browser cross-origin measurement access to one origin.
func (r *Registry) MountWithOrigin(parent context.Context, mux *http.ServeMux, origin string) {
	for path, e := range r.httpEndpoints {
		mux.Handle(path, httpAdapterWithOrigin(e, origin))
	}
	for path, e := range r.wsEndpoints {
		mux.Handle(path, wsAdapterWithOrigin(parent, e, origin))
	}
}

func wsAdapterWithOrigin(parent context.Context, e MessageHandler, allowedOrigin string) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if allowedOrigin != "" && r.Header.Get("Origin") != "" && r.Header.Get("Origin") != allowedOrigin {
			http.Error(w, "forbidden", http.StatusForbidden)
			return
		}
		var origins []string
		if allowedOrigin != "" {
			// allowedOrigin is the String() of a *url.URL the auth service already parsed at startup.
			u, _ := url.Parse(allowedOrigin)
			origins = []string{u.Host}
		}
		conn, err := websocket.Accept(w, r, &websocket.AcceptOptions{
			// Public mode is auth-less and cookie-less, holding no session state a forged origin could abuse.
			InsecureSkipVerify: allowedOrigin == "",
			OriginPatterns:     origins,
			CompressionMode:    websocket.CompressionDisabled,
		})
		if err != nil {
			return // Accept already wrote the handshake-failure response
		}
		// Bus frames are tiny text control messages (opcodes.go).
		conn.SetReadLimit(2048)
		// Accept hijacks the conn, which makes r.Context() unreliable (see its docs).
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

		bus := transport.NewWebSocketBus(ctx, conn)
		err = e.HandleMessages(ctx, bus)
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

func httpAdapterWithOrigin(e HTTPHandler, origin string) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		cors.Measurement(w.Header(), origin)
		if r.Method == http.MethodOptions {
			w.WriteHeader(http.StatusNoContent)
			return
		}
		if err := e.HandleHTTP(w, r); err != nil {
			http.Error(w, err.Error(), http.StatusInternalServerError)
		}
	})
}
