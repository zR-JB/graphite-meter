// Package static embeds and serves the built Svelte client.
package static

import (
	"bytes"
	"cmp"
	"crypto/sha256"
	"embed"
	"encoding/base64"
	"io"
	"io/fs"
	"net/http"
	"path"
	"slices"
	"strings"
)

//go:embed all:dist
var distFS embed.FS

// AppScriptCSPHash is the CSP 'sha256-...' digest of the single inline pre-paint <script> in the embedded index.html.
func AppScriptCSPHash() string {
	b, err := fs.ReadFile(distFS, "dist/index.html")
	if err != nil {
		return ""
	}
	return scriptCSPHash(b)
}

// scriptCSPHash returns the base64 sha256 of the one attribute-less inline <script>'s exact text.
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

// distRoot returns the embedded build rooted at dist/. fs.Sub rejects only a malformed path.
func distRoot() fs.FS {
	sub, err := fs.Sub(distFS, "dist")
	if err != nil {
		panic(err)
	}
	return sub
}

// Handler serves the embedded client as an SPA: a missing path falls back to index.html so client-side routing works.
func Handler() http.Handler {
	return handlerFor(distRoot())
}

// AuthenticatedHandler serves the same build as Handler, injecting a marker <meta> into index responses so the client.
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
		index = bytes.Replace(index, []byte("</head>"), slices.Concat(marker, []byte("</head>")), 1)
		w.Header().Set("Content-Type", "text/html; charset=utf-8")
		// The marked index differs from the public one for the same URL.
		w.Header().Set("Cache-Control", "no-store")
		// The response is already committed; a write failure is the client hanging up and there is nothing left to report.
		_, _ = w.Write(index)
	})
}

// handlerFor builds the SPA-fallback handler over fsys.
func handlerFor(fsys fs.FS) http.Handler {
	fileServer := http.FileServer(http.FS(fsys))

	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		name := strings.TrimPrefix(path.Clean(r.URL.Path), "/")
		name = cmp.Or(name, "index.html")
		if f, err := fsys.Open(name); err == nil {
			_ = f.Close()
			fileServer.ServeHTTP(w, r)
			return
		}
		// Serving index.html for a missing script fails strict MIME checks and the browser caches that failure.
		if isAssetPath(name) {
			http.NotFound(w, r)
			return
		}
		serveIndex(w, r, fsys)
	})
}

// isAssetPath reports whether name addresses a build file rather than a client route.
func isAssetPath(name string) bool {
	return path.Ext(name) != ""
}

// serveIndex writes fsys's index.html to w with http.ServeContent's caching/range/content-type handling.
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
