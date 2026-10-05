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
	"slices"
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

// PagePolicy is the shell's CSP; connect names peers beyond 'self', less IPv6 literals CSP cannot express.
func PagePolicy(connect []string) string {
	return pagePolicy(inlineScript, inlineStyle, slices.DeleteFunc(slices.Clone(connect),
		func(raw string) bool { return strings.Contains(raw, "://[") }))
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
	index, indexErr := fs.ReadFile(fsys, "index.html")
	index = bytes.Replace(index, []byte("</head>"), []byte(meta+"</head>"), 1)
	// Every embedded file by name with its content tag, so an unhashed file such as a font revalidates for free.
	tags := map[string]string{}
	_ = fs.WalkDir(fsys, ".", func(name string, entry fs.DirEntry, err error) error {
		if err == nil && !entry.IsDir() && name != "index.html" {
			b, _ := fs.ReadFile(fsys, name)
			sum := sha256.Sum256(b)
			tags[name] = `"` + base64.RawURLEncoding.EncodeToString(sum[:12]) + `"`
		}
		return nil
	})
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
		if _, ok := tags[name]; !ok {
			http.NotFound(w, r)
			return
		}
		h := w.Header()
		// The bundler names every file under assets/ by its content hash.
		if strings.HasPrefix(name, "assets/") {
			h.Set("Cache-Control", "public, max-age=31536000, immutable")
		} else {
			h.Set("Cache-Control", "no-cache")
		}
		// The build stores brotli and gzip copies of its text files beside them.
		served := name
		for _, coding := range [...]struct{ token, suffix string }{{"br", ".br"}, {"gzip", ".gz"}} {
			if _, ok := tags[name+coding.suffix]; ok {
				h.Set("Vary", "Accept-Encoding")
				if served == name && accepts(r.Header.Get("Accept-Encoding"), coding.token) {
					served = name + coding.suffix
					h.Set("Content-Encoding", coding.token)
				}
			}
		}
		file, err := fsys.Open(served)
		if err != nil {
			http.NotFound(w, r)
			return
		}
		defer file.Close()
		h.Set("ETag", tags[served])
		http.ServeContent(w, r, name, time.Time{}, file.(io.ReadSeeker))
	})
}

// accepts reports whether an Accept-Encoding header admits coding with a nonzero weight.
func accepts(header, coding string) bool {
	for part := range strings.SplitSeq(header, ",") {
		token, params, _ := strings.Cut(part, ";")
		if strings.EqualFold(strings.TrimSpace(token), coding) {
			q, err := strconv.ParseFloat(strings.TrimPrefix(strings.TrimSpace(params), "q="), 64)
			return err != nil || q > 0
		}
	}
	return false
}
