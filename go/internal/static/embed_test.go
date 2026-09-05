package static

import (
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
	"testing/fstest"
)

// testFS is a small in-memory dist fixture for embedded asset routing.
func testFS() fstest.MapFS {
	return fstest.MapFS{
		"index.html":             {Data: []byte("index page")},
		"assets/app.js":          {Data: []byte("console.log('app')")},
		"assets/sub/dir/file.js": {Data: []byte("console.log('nested')")},
	}
}

func TestHandlerRoutes(t *testing.T) {
	for _, test := range []struct {
		name, path, wantBody string
		fs                   fstest.MapFS
		wantStatus           int
	}{
		{name: "serves known asset", path: "/assets/app.js", wantStatus: http.StatusOK, wantBody: "console.log('app')"},
		{name: "root serves index", path: "/", wantStatus: http.StatusOK, wantBody: "index page"},
		{name: "unknown extensionless route is not an SPA fallback", path: "/results", wantStatus: http.StatusNotFound},
		{name: "missing asset with extension is 404", path: "/assets/missing.js", wantStatus: http.StatusNotFound},
		{name: "serves nested asset path", path: "/assets/sub/dir/file.js", wantStatus: http.StatusOK, wantBody: "console.log('nested')"},
		{name: "cleaned traversal cannot reach a second shell route", path: "/assets/../index.html", wantStatus: http.StatusNotFound},
		{name: "dot-segment path is not the shell", path: "/foo/..", wantStatus: http.StatusNotFound},
		{name: "asset dot-segment path is not the shell", path: "/assets/..", wantStatus: http.StatusNotFound},
		{name: "encoded dot-segment path is not the shell", path: "/foo/%2e%2e", wantStatus: http.StatusNotFound},
		{name: "backslash path is not the shell", path: `/foo\..\bar`, wantStatus: http.StatusNotFound},
		{name: "deep path traversal is not an SPA fallback", path: "/../../../etc/passwd", wantStatus: http.StatusNotFound},
		{name: "unknown trailing slash route is 404", path: "/settings/", wantStatus: http.StatusNotFound},
		{name: "index file is not a second shell route", path: "/index.html", wantStatus: http.StatusNotFound},
		{name: "empty FS falls back to 404", fs: fstest.MapFS{}, path: "/", wantStatus: http.StatusNotFound},
	} {
		t.Run(test.name, func(t *testing.T) {
			files := test.fs
			if files == nil {
				files = testFS()
			}
			rr := httptest.NewRecorder()
			handlerForWithMarker(files, resultHistoryMarker(false)).ServeHTTP(rr, httptest.NewRequest(http.MethodGet, test.path, nil))
			status, body := rr.Code, rr.Body.String()
			if status != test.wantStatus {
				t.Fatalf("status = %d, want %d", status, test.wantStatus)
			}
			if status == http.StatusNotFound && strings.Contains(body, "index page") {
				t.Fatal("missing path served the shell body")
			}
			if test.wantBody != "" && !strings.Contains(body, test.wantBody) {
				t.Fatalf("body = %q, want %q", body, test.wantBody)
			}
		})
	}
}

func TestHandlerHeadRequestMatchesGetHeaders(t *testing.T) {
	rr := httptest.NewRecorder()
	handlerForWithMarker(testFS(), resultHistoryMarker(false)).ServeHTTP(rr, httptest.NewRequest(http.MethodHead, "/assets/app.js", nil))
	if rr.Code != http.StatusOK {
		t.Fatalf("status = %d, want 200", rr.Code)
	}
	if rr.Body.Len() != 0 {
		t.Fatalf("HEAD body = %q, want empty", rr.Body.String())
	}
	if got := rr.Header().Get("Content-Length"); got != "18" {
		t.Fatalf("Content-Length = %q, want 18 (matching the GET body size)", got)
	}
}

func TestHandlerRejectsNonReadMethods(t *testing.T) {
	rr := httptest.NewRecorder()
	handlerForWithMarker(testFS(), resultHistoryMarker(false)).ServeHTTP(rr, httptest.NewRequest(http.MethodPost, "/", nil))
	if rr.Code != http.StatusMethodNotAllowed {
		t.Fatalf("POST / status = %d, want %d", rr.Code, http.StatusMethodNotAllowed)
	}
	if got := rr.Header().Get("Allow"); got != "GET, HEAD" {
		t.Fatalf("Allow = %q, want %q", got, "GET, HEAD")
	}
}

func TestScriptCSPHash(t *testing.T) {
	// The exact inline pre-paint script the client build emits.
	const inline = `try{e=localStorage.getItem("graphite-meter:v1"),t=e?JSON.parse(e).theme:null,r=t==="light"||t==="dark"?t:matchMedia("(prefers-color-scheme: light)").matches?"light":"dark",document.documentElement.setAttribute("data-theme",r)}catch(c){}var e,t,r;`
	html := []byte(`<!doctype html><head><style>x</style> <script>` + inline +
		`</script> <script type="module" src="/assets/app.js"></script></head>`)
	if got := scriptCSPHash(html); got != "i18M9x6p8PNJSBUDdO2pX/7us3FTrwpVfsQ1eUfPYqw=" {
		t.Fatalf("scriptCSPHash = %q, want the cross-checked digest", got)
	}
}

func TestScriptCSPHashHandlesNoInlineScript(t *testing.T) {
	// The tracked placeholder has no inline script, so the caller omits script-src.
	if got := scriptCSPHash([]byte(`<html><body>no scripts</body></html>`)); got != "" {
		t.Fatalf("scriptCSPHash without an inline script = %q, want empty", got)
	}
	// A module-only build (src attribute) must not match the bare delimiter.
	if got := scriptCSPHash([]byte(`<script src="/a.js"></script>`)); got != "" {
		t.Fatalf("scriptCSPHash of a src-only script = %q, want empty", got)
	}
}

func BenchmarkIndexResponse(b *testing.B) {
	files := fstest.MapFS{
		"index.html": {Data: []byte("<html><head></head><body>" + strings.Repeat("x", 16*1024) + "</body></html>")},
	}
	h := handlerForWithMarker(files, resultHistoryMarker(true))
	req := httptest.NewRequest(http.MethodGet, "/", nil)
	w := &indexBenchmarkWriter{header: make(http.Header)}
	b.ReportAllocs()
	for b.Loop() {
		h.ServeHTTP(w, req)
	}
}

type indexBenchmarkWriter struct{ header http.Header }

func (w *indexBenchmarkWriter) Header() http.Header         { return w.header }
func (w *indexBenchmarkWriter) WriteHeader(int)             {}
func (w *indexBenchmarkWriter) Write(p []byte) (int, error) { return len(p), nil }
