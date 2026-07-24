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

// testFS is a small in-memory dist fixture: one known asset, one index.html,
// enough to exercise handlerFor's routing without depending on any real
// committed dist content.
func testFS() fstest.MapFS {
	return fstest.MapFS{
		"index.html":             {Data: []byte("index page")},
		"assets/app.js":          {Data: []byte("console.log('app')")},
		"assets/sub/dir/file.js": {Data: []byte("console.log('nested')")},
	}
}

func get(t *testing.T, srv *httptest.Server, path string) (int, string) {
	t.Helper()
	resp, err := http.Get(srv.URL + path)
	if err != nil {
		t.Fatalf("GET %s: %v", path, err)
	}
	defer resp.Body.Close()
	body, err := io.ReadAll(resp.Body)
	if err != nil {
		t.Fatalf("read body for %s: %v", path, err)
	}
	return resp.StatusCode, string(body)
}

func TestHandlerServesKnownAsset(t *testing.T) {
	srv := httptest.NewServer(handlerFor(testFS()))
	defer srv.Close()

	status, body := get(t, srv, "/assets/app.js")
	if status != http.StatusOK {
		t.Fatalf("status = %d, want 200", status)
	}
	if !strings.Contains(body, "console.log('app')") {
		t.Fatalf("body = %q, want the asset content", body)
	}
}

func TestHandlerRootServesIndex(t *testing.T) {
	srv := httptest.NewServer(handlerFor(testFS()))
	defer srv.Close()

	status, body := get(t, srv, "/")
	if status != http.StatusOK {
		t.Fatalf("status = %d, want 200", status)
	}
	if !strings.Contains(body, "index page") {
		t.Fatalf("body = %q, want index.html content", body)
	}
}

func TestHandlerSPAFallbackForExtensionlessRoute(t *testing.T) {
	srv := httptest.NewServer(handlerFor(testFS()))
	defer srv.Close()

	// /results isn't in the fixture and has no extension: client-side
	// routing, so it must fall back to index.html rather than 404.
	status, body := get(t, srv, "/results")
	if status != http.StatusOK {
		t.Fatalf("status = %d, want 200 (SPA fallback)", status)
	}
	if !strings.Contains(body, "index page") {
		t.Fatalf("body = %q, want index.html content", body)
	}
}

func TestHandlerMissingAssetWithExtensionIs404(t *testing.T) {
	srv := httptest.NewServer(handlerFor(testFS()))
	defer srv.Close()

	// /assets/missing.js has an extension but isn't in the fixture: it must
	// 404, NOT fall back to index.html (a stale-build 404 must fail loudly,
	// not serve HTML for a requested script).
	status, _ := get(t, srv, "/assets/missing.js")
	if status != http.StatusNotFound {
		t.Fatalf("status = %d, want 404", status)
	}
}

func TestHandlerServesNestedAssetPath(t *testing.T) {
	srv := httptest.NewServer(handlerFor(testFS()))
	defer srv.Close()

	status, body := get(t, srv, "/assets/sub/dir/file.js")
	if status != http.StatusOK {
		t.Fatalf("status = %d, want 200", status)
	}
	if !strings.Contains(body, "console.log('nested')") {
		t.Fatalf("body = %q, want the nested asset content", body)
	}
}

func TestHandlerCleansPathTraversal(t *testing.T) {
	srv := httptest.NewServer(handlerFor(testFS()))
	defer srv.Close()

	// path.Clean collapses "/assets/../index.html" to "/index.html" before the
	// fs lookup, so a traversal attempt can only ever reach paths already
	// inside the fs root — it must never escape it or 500.
	status, body := get(t, srv, "/assets/../index.html")
	if status != http.StatusOK {
		t.Fatalf("status = %d, want 200", status)
	}
	if !strings.Contains(body, "index page") {
		t.Fatalf("body = %q, want index.html content", body)
	}
}

func TestHandlerDeepPathTraversalFallsBackToIndex(t *testing.T) {
	srv := httptest.NewServer(handlerFor(testFS()))
	defer srv.Close()

	// Enough ".." segments to walk above the fs root collapse under
	// path.Clean to a path outside the fixture, landing on the SPA fallback —
	// not an escape from the embedded fs, and (regression check) not a
	// redirect loop: disallow redirects so a loop fails the test instead of
	// hanging until the client's redirect cap trips.
	client := &http.Client{
		CheckRedirect: func(req *http.Request, via []*http.Request) error {
			return fmt.Errorf("unexpected redirect to %s", req.URL)
		},
	}
	resp, err := client.Get(srv.URL + "/../../../etc/passwd")
	if err != nil {
		t.Fatalf("GET: %v", err)
	}
	defer resp.Body.Close()
	body, err := io.ReadAll(resp.Body)
	if err != nil {
		t.Fatalf("read body: %v", err)
	}

	if resp.StatusCode != http.StatusOK {
		t.Fatalf("status = %d, want 200 (SPA fallback)", resp.StatusCode)
	}
	if !strings.Contains(string(body), "index page") {
		t.Fatalf("body = %q, want index.html content, not filesystem escape", body)
	}
}

func TestHandlerSPAFallbackForTrailingSlashRouteDoesNotRedirectLoop(t *testing.T) {
	srv := httptest.NewServer(handlerFor(testFS()))
	defer srv.Close()

	// A trailing-slash client route (e.g. "/settings/") is extensionless and
	// missing from the fixture, so it hits the same SPA-fallback branch as
	// "/results" above. It is the shape that redirect-loops if the fallback
	// ever delegates to http.FileServer: FileServer's "./" redirect is a fixed
	// point for a path already ending in "/". Disallow redirects so that fails
	// loudly instead of hanging until the client's redirect cap trips.
	client := &http.Client{
		CheckRedirect: func(req *http.Request, via []*http.Request) error {
			return fmt.Errorf("unexpected redirect to %s", req.URL)
		},
	}
	resp, err := client.Get(srv.URL + "/settings/")
	if err != nil {
		t.Fatalf("GET: %v", err)
	}
	defer resp.Body.Close()
	body, err := io.ReadAll(resp.Body)
	if err != nil {
		t.Fatalf("read body: %v", err)
	}

	if resp.StatusCode != http.StatusOK {
		t.Fatalf("status = %d, want 200 (SPA fallback)", resp.StatusCode)
	}
	if !strings.Contains(string(body), "index page") {
		t.Fatalf("body = %q, want index.html content", body)
	}
}

func TestHandlerEmptyFSFallsBackTo404(t *testing.T) {
	srv := httptest.NewServer(handlerFor(fstest.MapFS{}))
	defer srv.Close()

	// No index.html at all: root can't open, has no extension, so the SPA
	// fallback itself 404s instead of looping or panicking.
	status, _ := get(t, srv, "/")
	if status != http.StatusNotFound {
		t.Fatalf("status = %d, want 404", status)
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

func TestScriptCSPHash(t *testing.T) {
	// The exact inline pre-paint script the client build emits; its digest is
	// what a browser computes for the CSP 'sha256-…' source, cross-checked
	// independently. A change to the script must change this hash in lockstep.
	const inline = `try{e=localStorage.getItem("graphite-meter:v1"),t=e?JSON.parse(e).theme:null,r=t==="light"||t==="dark"?t:matchMedia("(prefers-color-scheme: light)").matches?"light":"dark",document.documentElement.setAttribute("data-theme",r)}catch(c){}var e,t,r;`
	html := []byte(`<!doctype html><head><style>x</style> <script>` + inline +
		`</script> <script type="module" src="/assets/app.js"></script></head>`)
	if got := scriptCSPHash(html); got != "i18M9x6p8PNJSBUDdO2pX/7us3FTrwpVfsQ1eUfPYqw=" {
		t.Fatalf("scriptCSPHash = %q, want the cross-checked digest", got)
	}
}

func TestScriptCSPHashHandlesNoInlineScript(t *testing.T) {
	// The tracked placeholder carries no inline script; the hash is then empty
	// and the caller omits script-src rather than pinning nothing.
	if got := scriptCSPHash([]byte(`<html><body>no scripts</body></html>`)); got != "" {
		t.Fatalf("scriptCSPHash without an inline script = %q, want empty", got)
	}
	// A module-only build (src attribute) must not match the bare delimiter.
	if got := scriptCSPHash([]byte(`<script src="/a.js"></script>`)); got != "" {
		t.Fatalf("scriptCSPHash of a src-only script = %q, want empty", got)
	}
}
