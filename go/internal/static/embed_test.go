package static

import (
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
		"index.html":    {Data: []byte("index page")},
		"assets/app.js": {Data: []byte("console.log('app')")},
	}
}

// get is a small helper: GETs path against the static handler and returns the
// status and body.
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
