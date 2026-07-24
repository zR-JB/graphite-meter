package static

import (
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
	h := handlerForAuthenticated(files)

	for _, tc := range []struct {
		name       string
		path       string
		wantMarker bool
	}{
		{"root serves the marked index", "/", true},
		{"client route falls back to the marked index", "/route", true},
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
