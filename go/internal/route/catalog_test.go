package route

import (
	"os"
	"strings"
	"testing"
)

func TestCatalogMatchesCrossLanguageRoutes(t *testing.T) {
	data, err := os.ReadFile("../../../api/routes.txt")
	if err != nil {
		t.Fatal(err)
	}
	seen := make(map[string]bool)
	for line := range strings.SplitSeq(string(data), "\n") {
		line = strings.TrimSpace(line)
		if line == "" || strings.HasPrefix(line, "#") {
			continue
		}
		fields := strings.Split(line, "|")
		if len(fields) != 3 {
			t.Fatalf("invalid route pin: %s", line)
		}
		path, kind := strings.TrimSpace(fields[1]), strings.TrimSpace(fields[2])
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
