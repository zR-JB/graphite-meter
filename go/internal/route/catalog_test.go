package route

import (
	"testing"

	"github.com/zR-JB/graphite-meter/go/internal/apipin"
)

func TestCatalogMatchesCrossLanguageRoutes(t *testing.T) {
	seen := make(map[string]bool)
	for _, row := range apipin.Rows(t, "routes.txt", 3) {
		path, kind := row[1], row[2]
		if seen[path] {
			t.Fatalf("duplicate pinned route %q", path)
		}
		seen[path] = true
		spec, ok := Lookup(path)
		if !ok || string(spec.Kind) != kind {
			t.Errorf("%s = %+v, present %v; want kind %s", path, spec, ok, kind)
		}
	}
	for path := range catalog {
		if !seen[path] {
			t.Errorf("unpublished measurement route %q", path)
		}
	}
	for _, path := range []string{"", "/", "/login", "/auth/cli/token", "/download/", "/wt/ping/", "/probe?x=1"} {
		if _, ok := Lookup(path); ok {
			t.Errorf("accepted non-measurement path %q", path)
		}
	}
}
