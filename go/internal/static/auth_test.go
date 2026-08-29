package static

import (
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
	"testing/fstest"
)

func TestAuthenticatedHandlerMarksOnlyIndexResponses(t *testing.T) {
	files := fstest.MapFS{
		"index.html": {Data: []byte("<html><head></head><body>app</body></html>")},
		"asset.js":   {Data: []byte("asset")},
	}
	h := handlerForWithMarker(files, []byte(`<meta name="graphite-meter-auth" content="enabled">`))

	for _, tc := range []struct {
		name       string
		path       string
		wantMarker bool
	}{
		{"root serves the marked index", "/", true},
		{"unknown client route is rejected", "/route", false},
		{"asset is served unmarked", "/asset.js", false},
	} {
		t.Run(tc.name, func(t *testing.T) {
			rr := httptest.NewRecorder()
			h.ServeHTTP(rr, httptest.NewRequest("GET", tc.path, nil))
			if got := strings.Contains(rr.Body.String(), "graphite-meter-auth"); got != tc.wantMarker {
				t.Fatalf("GET %s marker present = %v, want %v", tc.path, got, tc.wantMarker)
			}
		})
	}
}

func TestResultHistoryMetadataAndCachePolicy(t *testing.T) {
	files := fstest.MapFS{
		"index.html": {Data: []byte("<html><head></head><body>app</body></html>")},
		"asset.js":   {Data: []byte("asset")},
	}
	for _, tc := range []struct {
		name, marker string
	}{
		{"disabled", `content="false"`},
		{"enabled", `content="true"`},
	} {
		t.Run(tc.name, func(t *testing.T) {
			enabled := tc.name == "enabled"
			rr := httptest.NewRecorder()
			handlerForWithMarker(files, resultHistoryMarker(enabled)).ServeHTTP(rr, httptest.NewRequest(http.MethodGet, "/", nil))
			if !strings.Contains(rr.Body.String(), `name="graphite-meter-result-history-default"`) || !strings.Contains(rr.Body.String(), tc.marker) {
				t.Fatalf("body = %q, missing result-history marker %q", rr.Body.String(), tc.marker)
			}
			if got := rr.Header().Get("Cache-Control"); got != "no-store" {
				t.Fatalf("Cache-Control = %q, want no-store", got)
			}
		})
	}
}

func TestMarkedIndexHeadHasNoBody(t *testing.T) {
	files := fstest.MapFS{
		"index.html": {Data: []byte("<html><head></head><body>app</body></html>")},
	}
	rr := httptest.NewRecorder()
	handlerForWithMarker(files, resultHistoryMarker(true)).ServeHTTP(rr, httptest.NewRequest(http.MethodHead, "/", nil))
	if rr.Code != http.StatusOK {
		t.Fatalf("HEAD / status = %d, want %d", rr.Code, http.StatusOK)
	}
	if rr.Body.Len() != 0 {
		t.Fatalf("HEAD / wrote %d body bytes", rr.Body.Len())
	}
}
