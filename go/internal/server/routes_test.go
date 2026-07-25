package server

import (
	"bufio"
	"os"
	"strings"
	"testing"

	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

const routePinPath = "../../../api/routes.txt"

// pinnedRoute is one row of api/routes.txt: the path a route is mounted at and
// the transport that reaches it.
type pinnedRoute struct{ path, kind string }

// loadRoutePin parses api/routes.txt into name → route, skipping comment/blank
// lines. This is the same fixture the TS route table asserts against
// (client/src/lib/runner/real/routes.test.ts).
func loadRoutePin(t *testing.T) map[string]pinnedRoute {
	t.Helper()
	f, err := os.Open(routePinPath)
	if err != nil {
		t.Fatalf("open route pin: %v", err)
	}
	defer f.Close()

	pinned := make(map[string]pinnedRoute)
	scanner := bufio.NewScanner(f)
	lineNumber := 0
	for scanner.Scan() {
		lineNumber++
		line := strings.TrimSpace(scanner.Text())
		if line == "" || strings.HasPrefix(line, "#") {
			continue
		}
		fields := strings.Split(line, "|")
		if len(fields) != 3 {
			t.Fatalf("line %d: want 3 fields: %q", lineNumber, line)
		}
		kind := strings.TrimSpace(fields[2])
		switch kind {
		case "http", "ws", "wt":
		default:
			t.Fatalf("line %d: kind %q is not http, ws or wt", lineNumber, kind)
		}
		pinned[strings.TrimSpace(fields[0])] = pinnedRoute{path: strings.TrimSpace(fields[1]), kind: kind}
	}
	if err := scanner.Err(); err != nil {
		t.Fatalf("scan route pin: %v", err)
	}
	if len(pinned) == 0 {
		t.Fatal("route pin is empty: expected populated routes")
	}
	return pinned
}

// TestRoutesMatchPin is the byte-exact check both languages run against the same
// file: the paths this server mounts and the defaults a discovered target carries
// must be exactly the pinned set, no more and no less.
func TestRoutesMatchPin(t *testing.T) {
	pinned := loadRoutePin(t)
	throughput := wire.DefaultThroughputRoutes()
	latency := wire.DefaultLatencyRoutes()

	// Every Go constant that must hold the pinned path: what the mux mounts,
	// then the defaults a discovered target carries. The kind is how the
	// registry mounts it, so a route moved between mechanisms fails here.
	sites := map[string]struct {
		kind  string
		paths []string
	}{
		"probe":          {"http", []string{routeProbe, throughput.Probe, latency.Probe}},
		"download":       {"http", []string{routeDownload, throughput.Download}},
		"upload":         {"http", []string{routeUpload, throughput.Upload}},
		"uploadSession":  {"http", []string{routeUploadSession, throughput.UploadSession}},
		"uploadProgress": {"http", []string{routeUploadProgress, throughput.UploadProgress}},
		"ping":           {"ws", []string{routePing, latency.Ping}},
		"wtSession":      {"http", []string{routeWTSession, throughput.WTSession, latency.WTSession}},
		"wtDownload":     {"wt", []string{routeWTDownload, throughput.WTDownload}},
		"wtUpload":       {"wt", []string{routeWTUpload, throughput.WTUpload}},
		"wtPing":         {"wt", []string{routeWTPing, latency.WTPing}},
	}
	if len(sites) != len(pinned) {
		t.Errorf("Go declares %d routes; %d are pinned", len(sites), len(pinned))
	}
	for name, site := range sites {
		want, ok := pinned[name]
		if !ok {
			t.Errorf("%s: declared in Go but not pinned", name)
			continue
		}
		if site.kind != want.kind {
			t.Errorf("%s: Go mounts it as %q; pinned as %q", name, site.kind, want.kind)
		}
		for _, path := range site.paths {
			if path != want.path {
				t.Errorf("%s: Go has %q; pinned as %q", name, path, want.path)
			}
		}
	}
}
