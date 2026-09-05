package auth

import (
	"maps"
	"net/http"
	"net/http/httptest"
	"os"
	"slices"
	"strings"
	"testing"
)

const routePinPath = "../../../api/routes.txt"

var pinnedKinds = map[string]string{}

// pathsOfKind returns every pinned path reached by the named mechanism.
func pathsOfKind(t *testing.T, kind string) []string {
	t.Helper()
	loadRoutePin(t)
	var paths []string
	for path, k := range pinnedKinds {
		if k == kind {
			paths = append(paths, path)
		}
	}
	return slices.Sorted(slices.Values(paths))
}

func loadRoutePin(t *testing.T) map[string]string {
	t.Helper()
	raw, err := os.ReadFile(routePinPath)
	if err != nil {
		t.Fatalf("open route pin: %v", err)
	}
	pinned := make(map[string]string)
	for line := range strings.SplitSeq(string(raw), "\n") {
		line = strings.TrimSpace(line)
		if line == "" || strings.HasPrefix(line, "#") {
			continue
		}
		fields := strings.Split(line, "|")
		if len(fields) != 3 {
			t.Fatalf("want 3 fields: %q", line)
		}
		pinned[strings.TrimSpace(fields[0])] = strings.TrimSpace(fields[1])
		pinnedKinds[strings.TrimSpace(fields[1])] = strings.TrimSpace(fields[2])
	}
	if len(pinned) == 0 {
		t.Fatal("route pin is empty; expected populated routes")
	}
	return pinned
}

func TestAuthRoutesMatchPin(t *testing.T) {
	pinned := loadRoutePin(t)
	ping, ok := pinned["ping"]
	if !ok {
		t.Fatal("ping: not in the route pin")
	}
	measurement := slices.Sorted(maps.Values(pinned))
	methods := []string{http.MethodGet, http.MethodPost, http.MethodDelete, http.MethodConnect}

	t.Run("isMeasurementRoute", func(t *testing.T) {
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

	t.Run("isWebTransportRoute", func(t *testing.T) {
		sessions := pathsOfKind(t, "wt")
		for _, path := range sessions {
			if !isWebTransportRoute(path) {
				t.Errorf("isWebTransportRoute(%q) = false", path)
			}
		}
		// The mint is a plain POST, not a session upgrade.
		if isWebTransportRoute(pinned["wtSession"]) {
			t.Errorf("isWebTransportRoute(%q) = true", pinned["wtSession"])
		}
	})

	t.Run("validRequestOrigin", func(t *testing.T) {
		s := testService(t)
		_, sess, err := s.createSession("subject", "Name", "local")
		if err != nil {
			t.Fatal(err)
		}
		for _, path := range measurement {
			r := secureRequest(http.MethodGet, path, nil)
			r.Header.Set("Sec-Fetch-Site", "same-origin")
			if got, want := s.validRequestOrigin(r, Principal{session: sess}), path != ping; got != want {
				t.Errorf("validRequestOrigin(GET %q) without an Origin header = %v, want %v", path, got, want)
			}
		}
	})
}

func TestCORSPreflightPreservesExactRouteMethods(t *testing.T) {
	s := testService(t)
	methods := []string{http.MethodGet, http.MethodHead, http.MethodPost, http.MethodDelete, http.MethodConnect, http.MethodOptions, http.MethodPut, ""}
	allowed := map[string][]string{
		"/preflight": {http.MethodGet}, "/probe": {http.MethodGet}, "/download": {http.MethodGet},
		"/upload/session": {http.MethodPost}, "/upload": {http.MethodPost}, "/wt/session": {http.MethodPost},
		"/upload/progress": {http.MethodGet, http.MethodDelete}, "/ws/ping": {http.MethodGet},
		"/wt/download": {http.MethodConnect}, "/wt/upload": {http.MethodConnect}, "/wt/ping": {http.MethodConnect},
		"/login": nil, "/download/": nil,
	}
	for path, permitted := range allowed {
		for _, method := range methods {
			r := secureRequest(http.MethodOptions, path, nil)
			r.Header.Set("Origin", s.public.String())
			r.Header.Set("Access-Control-Request-Method", method)
			w := httptest.NewRecorder()
			s.corsPreflight(w, r, true)
			want := http.StatusForbidden
			if slices.Contains(permitted, method) {
				want = http.StatusNoContent
			}
			if w.Code != want {
				t.Errorf("preflight %s %s = %d, want %d", method, path, w.Code, want)
			}
		}
	}
}
