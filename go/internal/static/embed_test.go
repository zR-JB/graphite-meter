package static

import (
	"fmt"
	"io"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
	"testing/fstest"
)

// testFS is a small in-memory dist fixture for handlerFor routing.
func testFS() fstest.MapFS {
	return fstest.MapFS{
		"index.html":             {Data: []byte("index page")},
		"assets/app.js":          {Data: []byte("console.log('app')")},
		"assets/sub/dir/file.js": {Data: []byte("console.log('nested')")},
	}
}

// noRedirectClient rejects redirects so a redirect loop fails immediately.
func noRedirectClient() *http.Client {
	return &http.Client{
		CheckRedirect: func(req *http.Request, via []*http.Request) error {
			return fmt.Errorf("unexpected redirect to %s", req.URL)
		},
	}
}

func handlerResponse(t *testing.T, fs fstest.MapFS, client *http.Client, path string) (int, string) {
	t.Helper()
	if fs == nil {
		fs = testFS()
	}
	srv := httptest.NewServer(handlerFor(fs))
	defer srv.Close()
	if client == nil {
		client = http.DefaultClient
	}
	resp, err := client.Get(srv.URL + path)
	if err != nil {
		t.Fatalf("GET %s: %v", path, err)
	}
	defer resp.Body.Close()
	body, err := io.ReadAll(resp.Body)
	if err != nil {
		t.Fatalf("read body: %v", err)
	}
	return resp.StatusCode, string(body)
}

func TestHandlerRoutes(t *testing.T) {
	for _, test := range []struct {
		name, path, wantBody string
		fs                   fstest.MapFS
		client               *http.Client
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
		{name: "deep path traversal is not an SPA fallback", path: "/../../../etc/passwd", client: noRedirectClient(), wantStatus: http.StatusNotFound},
		{name: "unknown trailing slash route is 404", path: "/settings/", client: noRedirectClient(), wantStatus: http.StatusNotFound},
		{name: "index file is not a second shell route", path: "/index.html", wantStatus: http.StatusNotFound},
		{name: "empty FS falls back to 404", fs: fstest.MapFS{}, path: "/", wantStatus: http.StatusNotFound},
	} {
		t.Run(test.name, func(t *testing.T) {
			status, body := handlerResponse(t, test.fs, test.client, test.path)
			if status != test.wantStatus {
				t.Fatalf("status = %d, want %d", status, test.wantStatus)
			}
			if test.wantBody != "" && !strings.Contains(string(body), test.wantBody) {
				t.Fatalf("body = %q, want %q", body, test.wantBody)
			}
		})
	}
}

func TestHandlerDotSegmentsDoNotServeShell(t *testing.T) {
	for _, requestPath := range []string{"/foo/..", "/assets/.."} {
		status, body := handlerResponse(t, nil, noRedirectClient(), requestPath)
		if status != http.StatusNotFound {
			t.Errorf("%s status = %d, want 404", requestPath, status)
		}
		if strings.Contains(body, "index page") {
			t.Errorf("%s served the shell body", requestPath)
		}
	}
}

func TestHandlerHeadRequestMatchesGetHeaders(t *testing.T) {
	srv := httptest.NewServer(handlerFor(testFS()))
	defer srv.Close()

	resp, err := http.Head(srv.URL + "/assets/app.js")
	if err != nil {
		t.Fatalf("HEAD /assets/app.js: %v", err)
	}
	defer resp.Body.Close()

	if resp.StatusCode != http.StatusOK {
		t.Fatalf("status = %d, want 200", resp.StatusCode)
	}
	body, err := io.ReadAll(resp.Body)
	if err != nil {
		t.Fatalf("read body: %v", err)
	}
	if len(body) != 0 {
		t.Fatalf("HEAD body = %q, want empty", body)
	}
	if got := resp.Header.Get("Content-Length"); got != "18" {
		t.Fatalf("Content-Length = %q, want %q (matching the GET body size)", got, "18")
	}
}

func TestHandlerRejectsNonReadMethods(t *testing.T) {
	rr := httptest.NewRecorder()
	handlerFor(testFS()).ServeHTTP(rr, httptest.NewRequest(http.MethodPost, "/", nil))
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
