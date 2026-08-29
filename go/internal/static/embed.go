// Package static embeds and serves the built Svelte client.
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
	"slices"
	"strconv"
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

// Handler serves the public client while the browser owns all hash routes.
func Handler() http.Handler {
	return HandlerWithResultHistoryDefault(false)
}

// HandlerWithResultHistoryDefault adds the operator's local-history default to the public client metadata.
func HandlerWithResultHistoryDefault(resultHistoryDefault bool) http.Handler {
	return handlerForWithMarker(distRoot(), resultHistoryMarker(resultHistoryDefault))
}

// AuthenticatedHandlerWithResultHistoryDefault adds auth and local-history metadata to the client.
func AuthenticatedHandlerWithResultHistoryDefault(resultHistoryDefault bool) http.Handler {
	return handlerForWithMarker(distRoot(), slices.Concat(
		[]byte(`<meta name="graphite-meter-auth" content="enabled">`),
		resultHistoryMarker(resultHistoryDefault),
	))
}

func resultHistoryMarker(enabled bool) []byte {
	return []byte(`<meta name="graphite-meter-result-history-default" content="` + strconv.FormatBool(enabled) + `">`)
}

func handlerFor(fsys fs.FS) http.Handler {
	return handlerForWithMarker(fsys, nil)
}

// handlerForWithMarker serves the shell only at / and otherwise requires an embedded file.
func handlerForWithMarker(fsys fs.FS, marker []byte) http.Handler {
	fileServer := http.FileServer(http.FS(fsys))
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.Method != http.MethodGet && r.Method != http.MethodHead {
			w.Header().Set("Allow", "GET, HEAD")
			http.Error(w, "method not allowed", http.StatusMethodNotAllowed)
			return
		}
		name := strings.TrimPrefix(path.Clean(r.URL.Path), "/")
		if name == "" || name == "." {
			serveIndexWithMarker(w, r, fsys, marker)
			return
		}
		if name != "index.html" {
			if f, err := fsys.Open(name); err == nil {
				_ = f.Close()
				fileServer.ServeHTTP(w, r)
				return
			}
		}
		http.NotFound(w, r)
	})
}

func serveIndexWithMarker(w http.ResponseWriter, r *http.Request, fsys fs.FS, marker []byte) {
	if len(marker) != 0 {
		index, err := fs.ReadFile(fsys, "index.html")
		if err != nil {
			http.NotFound(w, r)
			return
		}
		index = bytes.Replace(index, []byte("</head>"), slices.Concat(marker, []byte("</head>")), 1)
		w.Header().Set("Content-Type", "text/html; charset=utf-8")
		w.Header().Set("Cache-Control", "no-store")
		w.Header().Set("Content-Length", strconv.Itoa(len(index)))
		if r.Method == http.MethodHead {
			return
		}
		_, _ = w.Write(index)
		return
	}
	serveIndex(w, r, fsys)
}

// serveIndex prevents caches from retaining operator-dependent metadata.
func serveIndex(w http.ResponseWriter, r *http.Request, fsys fs.FS) {
	w.Header().Set("Cache-Control", "no-store")
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
