// Package static embeds and serves the built Svelte client. The real client
// build (client/dist) is staged into ./dist at build time (justfile
// `_embed-client` / the Docker build); a tracked placeholder keeps //go:embed
// and `go build`/`go test` working without a client build present.
package static

import (
	"bytes"
	"crypto/sha256"
	"embed"
	"encoding/base64"
	"io"
	"io/fs"
	"net/http"
	"path"
	"strings"
)

//go:embed all:dist
var distFS embed.FS

// AppScriptCSPHash is the CSP 'sha256-...' digest of the single inline
// pre-paint <script> in the embedded index.html, "" for the tracked
// placeholder. It is computed from the embedded bytes, so it matches the page
// actually served.
func AppScriptCSPHash() string {
	b, err := fs.ReadFile(distFS, "dist/index.html")
	if err != nil {
		return ""
	}
	return scriptCSPHash(b)
}

// scriptCSPHash returns the base64 sha256 of the one attribute-less inline
// <script>'s exact text, the form a CSP 'sha256-...' source expects. The
// bundle's own module script carries a src attribute, so the bare "<script>"
// delimiter matches only the inline one.
func scriptCSPHash(html []byte) string {
	_, afterOpen, found := bytes.Cut(html, []byte("<script>"))
	if !found {
		return ""
	}
	content, _, found := bytes.Cut(afterOpen, []byte("</script>"))
	if !found {
		return ""
	}
	sum := sha256.Sum256(content)
	return base64.StdEncoding.EncodeToString(sum[:])
}

// distRoot returns the embedded build rooted at dist/. fs.Sub rejects only a
// malformed path, so a failure means this path and the //go:embed directive
// disagree.
func distRoot() fs.FS {
	sub, err := fs.Sub(distFS, "dist")
	if err != nil {
		panic(err)
	}
	return sub
}

// Handler serves the embedded client as an SPA: a missing path falls back to
// index.html so client-side routing works. With only the placeholder
// embedded (no real build), unknown paths 404 instead.
func Handler() http.Handler {
	return handlerFor(distRoot())
}

// AuthenticatedHandler serves the same build as Handler, injecting a marker
// <meta> into index responses so the client can tell auth is enabled. Requests
// for files pass through to the same routing, byte-identical.
func AuthenticatedHandler() http.Handler {
	return handlerForAuthenticated(distRoot())
}

func handlerForAuthenticated(fsys fs.FS) http.Handler {
	base := handlerFor(fsys)
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		name := strings.TrimPrefix(path.Clean(r.URL.Path), "/")
		if isAssetPath(name) {
			base.ServeHTTP(w, r)
			return
		}
		index, err := fs.ReadFile(fsys, "index.html")
		if err != nil {
			base.ServeHTTP(w, r)
			return
		}
		marker := []byte(`<meta name="graphite-meter-auth" content="enabled">`)
		index = bytes.Replace(index, []byte("</head>"), append(marker, []byte("</head>")...), 1)
		w.Header().Set("Content-Type", "text/html; charset=utf-8")
		// The marked index differs from the public one for the same URL, so it
		// must not be cached and later replayed under the other mode.
		w.Header().Set("Cache-Control", "no-store")
		// The response is already committed; a write failure is the client
		// hanging up and there is nothing left to report.
		_, _ = w.Write(index)
	})
}

// handlerFor builds the SPA-fallback handler over fsys. Taking an fs.FS lets
// tests exercise the routing against an in-memory fixture instead of the real
// embedded dist.
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
		// Serving index.html for a missing script fails strict MIME checks and
		// the browser caches that failure, making a stale build sticky.
		if isAssetPath(name) {
			http.NotFound(w, r)
			return
		}
		serveIndex(w, r, fsys)
	})
}

// isAssetPath reports whether name addresses a build file rather than a client
// route. Build files carry an extension, client routes do not.
func isAssetPath(name string) bool {
	return path.Ext(name) != ""
}

// serveIndex writes fsys's index.html to w with http.ServeContent's
// caching/range/content-type handling. It bypasses http.FileServer, which
// redirects any path ending in "index.html" to "./" and so loops forever on a
// path that resolves back to itself (a trailing slash route, or ".." segments).
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
