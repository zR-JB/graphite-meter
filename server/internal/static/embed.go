// Package static embeds and serves the built Svelte client. The real client
// build (client/dist) is staged into ./dist at build time (justfile
// `_stage-client` / the Docker build); a tracked placeholder keeps //go:embed
// and `go build`/`go test` working without a client build present.
package static

import (
	"embed"
	"io/fs"
	"net/http"
	"path"
	"strings"
)

//go:embed all:dist
var distFS embed.FS

// Handler serves the embedded client as an SPA: a request for a missing path
// falls back to index.html so client-side routing works. When only the
// placeholder is embedded (no real build), unknown paths 404 — acceptable for
// the skeleton; real runs stage the client first.
func Handler() http.Handler {
	sub, err := fs.Sub(distFS, "dist")
	if err != nil {
		panic(err)
	}
	fileServer := http.FileServer(http.FS(sub))

	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		name := strings.TrimPrefix(path.Clean(r.URL.Path), "/")
		if name == "" {
			name = "index.html"
		}
		if f, err := sub.Open(name); err == nil {
			_ = f.Close()
			fileServer.ServeHTTP(w, r)
			return
		}
		// SPA fallback: serve index.html for unknown (non-asset) routes.
		r2 := r.Clone(r.Context())
		r2.URL.Path = "/index.html"
		fileServer.ServeHTTP(w, r2)
	})
}
