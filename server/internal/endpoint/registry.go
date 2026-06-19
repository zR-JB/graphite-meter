package endpoint

import (
	"net/http"

	"github.com/zR-JB/graphite-meter/server/internal/transport"
)

// Registry maps paths to endpoints. The HTTP mux is built by walking it; the
// WebTransport dispatcher (Stage 5) resolves from the same registry. Adding an
// endpoint is a Register call — no listener code changes.
type Registry struct {
	httpEndpoints map[string]Endpoint
}

// NewRegistry returns an empty registry.
func NewRegistry() *Registry {
	return &Registry{httpEndpoints: make(map[string]Endpoint)}
}

// RegisterHTTP mounts an endpoint as an HTTP request/response handler at path.
func (r *Registry) RegisterHTTP(path string, e Endpoint) {
	r.httpEndpoints[path] = e
}

// Mount attaches all registered HTTP endpoints onto mux.
func (r *Registry) Mount(mux *http.ServeMux) {
	for path, e := range r.httpEndpoints {
		mux.Handle(path, httpAdapter(e))
	}
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
// client can measure cross-origin (app on :8080, measuring against :8443) with
// accurate Resource Timing.
func setCommonHeaders(w http.ResponseWriter) {
	h := w.Header()
	h.Set("Access-Control-Allow-Origin", "*")
	h.Set("Access-Control-Allow-Methods", "GET, POST, OPTIONS")
	h.Set("Access-Control-Allow-Headers", "*")
	h.Set("Timing-Allow-Origin", "*")
}
