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
	"time"
)

//go:embed all:dist
var distFS embed.FS

// The CSP digests of index.html's inline pre-paint <script> and <style>; both are empty without a build.
var inlineScript, inlineStyle = inlineHash("script"), inlineHash("style")

func inlineHash(tag string) string {
	b, _ := fs.ReadFile(distFS, "dist/index.html")
	return inlineCSPHash(b, tag)
}

// inlineCSPHash returns the base64 sha256 of the first attribute-less inline tag's exact text.
func inlineCSPHash(html []byte, tag string) string {
	_, afterOpen, opened := bytes.Cut(html, []byte("<"+tag+">"))
	content, _, closed := bytes.Cut(afterOpen, []byte("</"+tag+">"))
	if !opened || !closed {
		return ""
	}
	sum := sha256.Sum256(content)
	return base64.StdEncoding.EncodeToString(sum[:])
}

// PagePolicy is the client shell's Content-Security-Policy; connect names the peers it reaches beyond 'self'.
func PagePolicy(connect []string) string {
	return pagePolicy(inlineScript, inlineStyle, connect)
}

func pagePolicy(script, style string, connect []string) string {
	sources := func(directive, hash string) string {
		if hash == "" {
			return directive + " 'self'"
		}
		return directive + " 'self' 'sha256-" + hash + "'"
	}
	return strings.Join([]string{
		"default-src 'self'",
		sources("script-src", script),
		sources("style-src", style),
		"img-src 'self' data:",
		"font-src 'self'",
		"worker-src 'self'",
		"object-src 'none'",
		"base-uri 'none'",
		"form-action 'self'",
		"frame-ancestors 'none'",
		strings.Join(append([]string{"connect-src 'self'"}, connect...), " "),
	}, "; ")
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
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.Method != http.MethodGet && r.Method != http.MethodHead {
			w.Header().Set("Allow", "GET, HEAD")
			http.Error(w, "method not allowed", http.StatusMethodNotAllowed)
			return
		}
		if r.URL.Path == "/" && indexErr == nil {
			w.Header().Set("Content-Type", "text/html; charset=utf-8")
			w.Header().Set("Cache-Control", "no-store")
			http.ServeContent(w, r, "index.html", time.Time{}, bytes.NewReader(index))
			return
		}
		name := strings.TrimPrefix(r.URL.Path, "/")
		if fs.ValidPath(name) && name != "." && name != "index.html" {
			if info, err := fs.Stat(fsys, name); err == nil && !info.IsDir() {
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
