package static

import (
	"net/http"
	"net/http/httptest"
	"slices"
	"strings"
	"testing"
	"testing/fstest"

	"github.com/zR-JB/graphite-meter/go/internal/testkit"
)

func testFS() fstest.MapFS {
	return fstest.MapFS{
		"index.html":             {Data: []byte("<html><head></head><body>index page</body></html>")},
		"version.json":           {Data: []byte("{}")},
		"assets/app.js":          {Data: []byte("console.log('app')")},
		"assets/sub/dir/file.js": {Data: []byte("console.log('nested')")},
	}
}

func serve(h http.Handler, method, path string) *httptest.ResponseRecorder {
	rr := testkit.Record(h.ServeHTTP, httptest.NewRequest(method, path, nil))
	return rr
}

// Hashed bundle files never change under their name; the shell and unhashed files must revalidate.
func TestHandlerRoutes(t *testing.T) {
	const immutable = "public, max-age=31536000, immutable"
	for _, test := range []struct {
		name, path, wantBody, wantCache string
		fs                              fstest.MapFS
		wantStatus                      int
	}{
		{name: "known asset", path: "/assets/app.js", wantStatus: http.StatusOK, wantBody: "console.log('app')",
			wantCache: immutable},
		{name: "root serves index", path: "/", wantStatus: http.StatusOK, wantBody: "index page",
			wantCache: "no-store"},
		{name: "unhashed file", path: "/version.json", wantStatus: http.StatusOK, wantBody: "{}"},
		{name: "no SPA fallback", path: "/results", wantStatus: http.StatusNotFound},
		{name: "missing asset", path: "/assets/missing.js", wantStatus: http.StatusNotFound},
		{name: "nested asset", path: "/assets/sub/dir/file.js", wantStatus: http.StatusOK, wantBody: "nested",
			wantCache: immutable},
		{name: "cleaned traversal", path: "/assets/../index.html", wantStatus: http.StatusNotFound},
		{name: "dot segment", path: "/foo/..", wantStatus: http.StatusNotFound},
		{name: "asset dot segment", path: "/assets/..", wantStatus: http.StatusNotFound},
		{name: "encoded dot segment", path: "/foo/%2e%2e", wantStatus: http.StatusNotFound},
		{name: "backslash", path: `/foo\..\bar`, wantStatus: http.StatusNotFound},
		{name: "deep traversal", path: "/../../../etc/passwd", wantStatus: http.StatusNotFound},
		{name: "trailing slash", path: "/settings/", wantStatus: http.StatusNotFound},
		{name: "directory", path: "/assets", wantStatus: http.StatusNotFound},
		{name: "directory listing", path: "/assets/", wantStatus: http.StatusNotFound},
		{name: "index file is not a second shell", path: "/index.html", wantStatus: http.StatusNotFound},
		{name: "empty FS", fs: fstest.MapFS{}, path: "/", wantStatus: http.StatusNotFound},
	} {
		t.Run(test.name, func(t *testing.T) {
			files := test.fs
			if files == nil {
				files = testFS()
			}
			rr := serve(handler(files, false, false), http.MethodGet, test.path)
			body := rr.Body.String()
			cache := rr.Header().Get("Cache-Control")
			if rr.Code != test.wantStatus || rr.Code == http.StatusNotFound && strings.Contains(body, "index page") ||
				!strings.Contains(body, test.wantBody) || rr.Code == http.StatusOK && cache != test.wantCache {
				t.Fatalf("status = %d body = %q cache %q, want %d %q %q", rr.Code, body, cache, test.wantStatus,
					test.wantBody, test.wantCache)
			}
		})
	}
}

func TestHandlerMethods(t *testing.T) {
	h := handler(testFS(), false, true)
	for _, path := range []string{"/assets/app.js", "/"} {
		get, head := serve(h, http.MethodGet, path), serve(h, http.MethodHead, path)
		if head.Code != http.StatusOK || head.Body.Len() != 0 || get.Header().Get("Content-Length") == "" ||
			head.Header().Get("Content-Length") != get.Header().Get("Content-Length") {
			t.Fatalf("HEAD %s = %d with %d body bytes and length %q, GET length %q", path, head.Code, head.Body.Len(),
				head.Header().Get("Content-Length"), get.Header().Get("Content-Length"))
		}
	}
	rr := serve(h, http.MethodPost, "/")
	if rr.Code != http.StatusMethodNotAllowed || rr.Header().Get("Allow") != "GET, HEAD" {
		t.Fatalf("POST / = %d Allow %q, want 405 GET, HEAD", rr.Code, rr.Header().Get("Allow"))
	}
}

// Only the shell carries the authentication marker and the operator's result-history default.
func TestShellMetadata(t *testing.T) {
	for _, tc := range []struct {
		authenticated, history bool
		want                   []string
	}{
		{false, false, []string{`content="false"`}},
		{true, true, []string{`name="graphite-meter-auth"`, `content="true"`}},
	} {
		h := handler(testFS(), tc.authenticated, tc.history)
		shell := serve(h, http.MethodGet, "/").Body.String()
		for _, want := range append(tc.want, `name="graphite-meter-result-history-default"`) {
			if !strings.Contains(shell, want) {
				t.Errorf("shell %q lacks %s", shell, want)
			}
		}
		if strings.Contains(shell, "graphite-meter-auth") != tc.authenticated {
			t.Errorf("authenticated=%t shell = %q", tc.authenticated, shell)
		}
		asset := serve(h, http.MethodGet, "/assets/app.js").Body.String()
		if strings.Contains(asset, "graphite-meter") {
			t.Errorf("asset was marked: %q", asset)
		}
	}
}

// The browser suite fails on any violation of the permissive directives; only the restrictive ones need pinning.
func TestPagePolicy(t *testing.T) {
	built := strings.Split(PagePolicy([]string{"https://meter.example:*", "https://[2001:db8::2]:7248"}), "; ")
	for _, want := range []string{"object-src 'none'", "base-uri 'none'", "connect-src 'self' https://meter.example:*"} {
		if !slices.Contains(built, want) {
			t.Errorf("policy lacks %q, or kept an IPv6 literal CSP cannot express: %s", want, built)
		}
	}
}
