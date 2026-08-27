package server

import (
	"bufio"
	"os"
	"strings"
	"testing"

	"github.com/quic-go/webtransport-go"
	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

const routePinPath = "../../../api/routes.txt"

// pinnedRoute is one row of api/routes.txt: the path a route is mounted at and the transport that reaches it.
type pinnedRoute struct{ path, kind string }

// loadRoutePin parses api/routes.txt into name → route, skipping comment/blank lines.
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

func TestRoutesMatchPin(t *testing.T) {
	pinned := loadRoutePin(t)
	throughput := wire.DefaultThroughputRoutes()
	latency := wire.DefaultLatencyRoutes()

	// Every Go constant that must hold the pinned path: what the mux mounts, then the defaults a discovered target.
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
			t.Errorf("%s: Go declares it as %q; pinned as %q", name, site.kind, want.kind)
		}
		for _, path := range site.paths {
			if path != want.path {
				t.Errorf("%s: Go has %q; pinned as %q", name, path, want.path)
			}
		}
	}
}

func TestRouteKindsMatchTheRegistry(t *testing.T) {
	pinned := loadRoutePin(t)
	// One listener carrying every mechanism, so each pinned route is registered.
	reg := buildRegistry(&endpoints{}, muxTopology{
		discovery: true, latency: true, transfers: true, wt: &webtransport.Server{},
	}, nil)
	mounted := reg.Kinds()

	byPath := make(map[string]string, len(pinned))
	for name, route := range pinned {
		byPath[route.path] = name
	}
	for path, kind := range mounted {
		name, ok := byPath[path]
		if !ok {
			if path == "/preflight" {
				continue // advertised by every language, never a client route
			}
			t.Errorf("%s is registered but not pinned", path)
			continue
		}
		if pinned[name].kind != kind {
			t.Errorf("%s: registered as %q; pinned as %q", name, kind, pinned[name].kind)
		}
	}
	for name, route := range pinned {
		if _, ok := mounted[route.path]; !ok {
			t.Errorf("%s (%s) is pinned but never registered", name, route.path)
		}
	}
}
