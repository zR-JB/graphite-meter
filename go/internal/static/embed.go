// Package static embeds and serves the built Svelte client.
package static

import (
	"bytes"
	"crypto/sha256"
	"embed"
	"encoding/base64"
	"io/fs"
	"net/http"
	"strconv"
	"strings"
)

//go:embed all:dist
var distFS embed.FS

// AppScriptCSPHash is the CSP 'sha256-...' digest of the single inline pre-paint <script> in the embedded index.html.
func AppScriptCSPHash() string {
	b, _ := fs.ReadFile(distFS, "dist/index.html")
	return scriptCSPHash(b)
}

// scriptCSPHash returns the base64 sha256 of the one attribute-less inline <script>'s exact text.
func scriptCSPHash(html []byte) string {
	_, afterOpen, opened := bytes.Cut(html, []byte("<script>"))
	content, _, closed := bytes.Cut(afterOpen, []byte("</script>"))
	if !opened || !closed {
		return ""
	}
	sum := sha256.Sum256(content)
	return base64.StdEncoding.EncodeToString(sum[:])
}

// Handler serves the client shell at / and otherwise only embedded files. The shell carries the
// authentication marker and the operator's result-history default.
func Handler(authenticated, resultHistoryDefault bool) http.Handler {
	dist, _ := fs.Sub(distFS, "dist")
	return handler(dist, authenticated, resultHistoryDefault)
}

func handler(fsys fs.FS, authenticated, resultHistoryDefault bool) http.Handler {
	meta := `<meta name="graphite-meter-result-history-default" content="` +
		strconv.FormatBool(resultHistoryDefault) + `">`
	if authenticated {
		meta = `<meta name="graphite-meter-auth" content="enabled">` + meta
	}
	fileServer := http.FileServerFS(fsys)
	index, indexErr := fs.ReadFile(fsys, "index.html")
	index = bytes.Replace(index, []byte("</head>"), []byte(meta+"</head>"), 1)
	indexLength := strconv.Itoa(len(index))
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.Method != http.MethodGet && r.Method != http.MethodHead {
			w.Header().Set("Allow", "GET, HEAD")
			http.Error(w, "method not allowed", http.StatusMethodNotAllowed)
			return
		}
		if r.URL.Path == "/" && indexErr == nil {
			w.Header().Set("Content-Type", "text/html; charset=utf-8")
			w.Header().Set("Cache-Control", "no-store")
			w.Header().Set("Content-Length", indexLength)
			if r.Method != http.MethodHead {
				_, _ = w.Write(index)
			}
			return
		}
		name := strings.TrimPrefix(r.URL.Path, "/")
		if fs.ValidPath(name) && name != "." && name != "index.html" {
			if f, err := fsys.Open(name); err == nil {
				_ = f.Close()
				if strings.HasPrefix(name, "assets/") {
					// The bundler names every file under assets/ by its content hash.
					w.Header().Set("Cache-Control", "public, max-age=31536000, immutable")
				}
				fileServer.ServeHTTP(w, r)
				return
			}
		}
		http.NotFound(w, r)
	})
}
