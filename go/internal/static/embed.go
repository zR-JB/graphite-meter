// Package static embeds and serves the built Svelte client. The real client
// build (client/dist) is staged into ./dist at build time (justfile
// `_embed-client` / the Docker build); a tracked placeholder keeps //go:embed
// and `go build`/`go test` working without a client build present.
package static

import (
	"embed"
	"io"
	"io/fs"
	"net/http"
	"path"
	"strings"
)

//go:embed all:dist
var distFS embed.FS

// Handler serves the embedded client as an SPA: a missing path falls back to
// index.html so client-side routing works. With only the placeholder
// embedded (no real build), unknown paths 404 instead.
func Handler() http.Handler {
	sub, err := fs.Sub(distFS, "dist")
	if err != nil {
		panic(err)
	}
	return handlerFor(sub)
}

// handlerFor builds the SPA-fallback handler over fsys. Split out from
// Handler so tests can exercise the routing logic against an in-memory fs.FS
// instead of the real embedded dist.
func handlerFor(fsys fs.FS) http.Handler {
	fileServer := http.FileServer(http.FS(fsys))

	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		name := strings.TrimPrefix(path.Clean(r.URL.Path), "/")
		if name == "" {
			name = "index.html"
		}
		if f, err := fsys.Open(name); err == nil {
			_ = f.Close()
			fileServer.ServeHTTP(w, r)
			return
		}
		// A missing path with an extension (e.g. a content-hashed /assets/*.js)
		// must 404, not fall back to index.html: serving HTML for a missing
		// script fails strict MIME checks and the browser caches that failure,
		// turning a stale build into a sticky error.
		if path.Ext(name) != "" {
			http.NotFound(w, r)
			return
		}
		// SPA fallback: serve index.html's content for unknown (extensionless)
		// routes so client-side routing works. Served directly via
		// ServeContent rather than rewriting the request path and delegating
		// to fileServer: FileServer redirects any request whose path ends in
		// "index.html" to "./" to canonicalize the URL, which loops forever
		// once the path already resolves back to itself (e.g. a trailing
		// slash route, or ".." segments collapsing to one).
		serveIndex(w, r, fsys)
	})
}

// serveIndex writes fsys's index.html to w, matching http.ServeContent's
// caching/range/content-type handling without going through fileServer.
func serveIndex(w http.ResponseWriter, r *http.Request, fsys fs.FS) {
	f, err := fsys.Open("index.html")
	if err != nil {
		http.NotFound(w, r)
		return
	}
	defer f.Close()

	stat, err := f.Stat()
	if err != nil {
		http.NotFound(w, r)
		return
	}
	rs, ok := f.(io.ReadSeeker)
	if !ok {
		http.NotFound(w, r)
		return
	}
	http.ServeContent(w, r, "index.html", stat.ModTime(), rs)
}
