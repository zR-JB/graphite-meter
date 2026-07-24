package auth

import (
	"go/ast"
	"go/parser"
	"go/token"
	"maps"
	"net/http"
	"os"
	"slices"
	"strconv"
	"strings"
	"testing"
	"time"
)

const routePinPath = "../../../api/routes.txt"

// preflightPath is the one measurement-adjacent path outside the pin: discovery
// is served by every listener but requested through no route table, so the
// enumerations carry it and this test names it.
const preflightPath = "/preflight"

// loadRoutePin parses api/routes.txt into name → path. The parser is a copy of
// the one in go/internal/server/routes_test.go because auth cannot import
// server without inverting the dependency.
func loadRoutePin(t *testing.T) map[string]string {
	t.Helper()
	raw, err := os.ReadFile(routePinPath)
	if err != nil {
		t.Fatalf("open route pin: %v", err)
	}
	pinned := make(map[string]string)
	for _, line := range strings.Split(string(raw), "\n") {
		line = strings.TrimSpace(line)
		if line == "" || strings.HasPrefix(line, "#") {
			continue
		}
		name, path, ok := strings.Cut(line, "|")
		if !ok {
			t.Fatalf("want 2 fields: %q", line)
		}
		pinned[strings.TrimSpace(name)] = strings.TrimSpace(path)
	}
	if len(pinned) == 0 {
		t.Fatal("route pin is empty; expected populated routes")
	}
	return pinned
}

// enumeratedPaths returns every "/"-prefixed string literal in fn's body, which
// is how each enforcement site spells the paths it recognises.
func enumeratedPaths(t *testing.T, file, fn string) []string {
	t.Helper()
	f, err := parser.ParseFile(token.NewFileSet(), file, nil, 0)
	if err != nil {
		t.Fatalf("parse %s: %v", file, err)
	}
	for _, d := range f.Decls {
		fd, ok := d.(*ast.FuncDecl)
		if !ok || fd.Name.Name != fn {
			continue
		}
		var found []string
		ast.Inspect(fd.Body, func(n ast.Node) bool {
			lit, ok := n.(*ast.BasicLit)
			if !ok || lit.Kind != token.STRING {
				return true
			}
			if v, err := strconv.Unquote(lit.Value); err == nil && strings.HasPrefix(v, "/") {
				found = append(found, v)
			}
			return true
		})
		return found
	}
	t.Fatalf("%s: no func %s", file, fn)
	return nil
}

// assertEnumerates reports drift in both directions: a path the pin carries and
// the site dropped, and a path the site recognises that nothing pins.
func assertEnumerates(t *testing.T, site string, want, got []string) {
	t.Helper()
	have := make(map[string]bool, len(got))
	for _, path := range got {
		have[path] = true
	}
	for _, path := range want {
		if !have[path] {
			t.Errorf("%s: %q is pinned but not enumerated", site, path)
		}
		delete(have, path)
	}
	for _, path := range slices.Sorted(maps.Keys(have)) {
		t.Errorf("%s: enumerates %q, which is not pinned", site, path)
	}
}

// TestAuthRoutesMatchPin holds the enforcement boundary's own route
// enumerations to api/routes.txt. auth cannot import the package that mounts
// the routes, so the pin file is the only thing keeping the lists aligned, and
// a path the boundary does not recognise is a path it does not protect.
func TestAuthRoutesMatchPin(t *testing.T) {
	pinned := loadRoutePin(t)
	ping, ok := pinned["ping"]
	if !ok {
		t.Fatal("ping: not in the route pin")
	}
	measurement := append(slices.Sorted(maps.Values(pinned)), preflightPath)
	methods := []string{http.MethodGet, http.MethodPost, http.MethodDelete}

	t.Run("isMeasurementRoute", func(t *testing.T) {
		assertEnumerates(t, "wrap.go isMeasurementRoute", measurement, enumeratedPaths(t, "wrap.go", "isMeasurementRoute"))
		for _, path := range measurement {
			if !isMeasurementRoute(path) {
				t.Errorf("isMeasurementRoute(%q) = false", path)
			}
		}
		if isMeasurementRoute("/login") {
			t.Error(`isMeasurementRoute("/login") = true`)
		}
	})

	t.Run("allowedCORSMethod", func(t *testing.T) {
		assertEnumerates(t, "headers.go allowedCORSMethod", measurement, enumeratedPaths(t, "headers.go", "allowedCORSMethod"))
		for _, path := range measurement {
			if !slices.ContainsFunc(methods, func(m string) bool { return allowedCORSMethod(path, m) }) {
				t.Errorf("allowedCORSMethod(%q): no method allowed", path)
			}
		}
		for _, m := range methods {
			if allowedCORSMethod("/login", m) {
				t.Errorf("allowedCORSMethod(%q, %q) = true", "/login", m)
			}
		}
	})

	t.Run("validRequestOrigin", func(t *testing.T) {
		assertEnumerates(t, "trust.go wsPingOriginAllowed", []string{ping}, enumeratedPaths(t, "trust.go", "wsPingOriginAllowed"))

		s := testService(t)
		_, sess, err := s.createSession("subject", "Name", "local", time.Time{})
		if err != nil {
			t.Fatal(err)
		}
		// The ping route demands the public Origin outright; the rest of the
		// measurement set settles for a same-origin fetch metadata claim.
		for _, path := range measurement {
			r := secureRequest(http.MethodGet, path, nil)
			r.Header.Set("Sec-Fetch-Site", "same-origin")
			if got, want := s.validRequestOrigin(r, Principal{session: sess}), path != ping; got != want {
				t.Errorf("validRequestOrigin(GET %q) without an Origin header = %v, want %v", path, got, want)
			}
		}
	})
}
