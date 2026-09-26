package static

import (
	"net/http"
	"net/http/httptest"
	"slices"
	"strings"
	"testing"
	"testing/fstest"
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
	rr := httptest.NewRecorder()
	h.ServeHTTP(rr, httptest.NewRequest(method, path, nil))
	return rr
}

func TestHandlerRoutes(t *testing.T) {
	for _, test := range []struct {
		name, path, wantBody string
		fs                   fstest.MapFS
		wantStatus           int
	}{
		{name: "known asset", path: "/assets/app.js", wantStatus: http.StatusOK, wantBody: "console.log('app')"},
		{name: "root serves index", path: "/", wantStatus: http.StatusOK, wantBody: "index page"},
		{name: "no SPA fallback", path: "/results", wantStatus: http.StatusNotFound},
		{name: "missing asset", path: "/assets/missing.js", wantStatus: http.StatusNotFound},
		{name: "nested asset", path: "/assets/sub/dir/file.js", wantStatus: http.StatusOK, wantBody: "nested"},
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
			if rr.Code != test.wantStatus || rr.Code == http.StatusNotFound && strings.Contains(body, "index page") ||
				!strings.Contains(body, test.wantBody) {
				t.Fatalf("status = %d body = %q, want %d %q", rr.Code, body, test.wantStatus, test.wantBody)
			}
		})
	}
}

// Hashed bundle files never change under their name; the shell and unhashed files must revalidate.
func TestOnlyHashedAssetsAreImmutable(t *testing.T) {
	for path, want := range map[string]string{
		"/assets/app.js": "public, max-age=31536000, immutable",
		"/version.json":  "",
		"/":              "no-store",
	} {
		if rr := serve(handler(testFS(), false, false), http.MethodGet, path); rr.Code != http.StatusOK ||
			rr.Header().Get("Cache-Control") != want {
			t.Errorf("GET %s = %d Cache-Control %q, want 200 %q", path, rr.Code, rr.Header().Get("Cache-Control"), want)
		}
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
	built := strings.Split(pagePolicy("S", "T", []string{"https://meter.example:*"}), "; ")
	for _, want := range []string{
		"default-src 'self'", "object-src 'none'", "base-uri 'none'", "form-action 'self'", "frame-ancestors 'none'",
	} {
		if !slices.Contains(built, want) {
			t.Errorf("policy lacks %q: %s", want, built)
		}
	}
	if bare := pagePolicy("", "", nil); strings.Contains(bare, "sha256") ||
		!strings.HasSuffix(bare, "; connect-src 'self'") {
		t.Errorf("policy without a build = %s", bare)
	}
}
