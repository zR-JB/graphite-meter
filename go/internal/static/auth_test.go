package static

import (
	"io/fs"
	"net/http/httptest"
	"strings"
	"testing"
	"testing/fstest"
)

func TestAuthenticatedHandlerInjectsOnlyIndex(t *testing.T) {
	files := fstest.MapFS{"index.html": {Data: []byte("<html><head></head><body>app</body></html>")}, "asset.js": {Data: []byte("asset")}}
	h := handlerForAuthenticated(fs.FS(files))
	for _, tc := range []struct {
		path   string
		marker bool
	}{{"/", true}, {"/route", true}, {"/asset.js", false}} {
		rr := httptest.NewRecorder()
		h.ServeHTTP(rr, httptest.NewRequest("GET", tc.path, nil))
		got := strings.Contains(rr.Body.String(), "graphite-meter-auth")
		if got != tc.marker {
			t.Errorf("%s marker=%v", tc.path, got)
		}
	}
}
