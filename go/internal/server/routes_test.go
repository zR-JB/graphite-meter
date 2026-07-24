package server

import (
	"bufio"
	"os"
	"strings"
	"testing"

	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

const routePinPath = "../../../api/routes.txt"

// loadRoutePin parses api/routes.txt into name → path, skipping comment/blank
// lines. This is the same fixture the TS route table asserts against
// (client/src/lib/runner/real/routes.test.ts).
func loadRoutePin(t *testing.T) map[string]string {
	t.Helper()
	f, err := os.Open(routePinPath)
	if err != nil {
		t.Fatalf("open route pin: %v", err)
	}
	defer f.Close()

	pinned := make(map[string]string)
	scanner := bufio.NewScanner(f)
	lineNumber := 0
	for scanner.Scan() {
		lineNumber++
		line := strings.TrimSpace(scanner.Text())
		if line == "" || strings.HasPrefix(line, "#") {
			continue
		}
		name, path, ok := strings.Cut(line, "|")
		if !ok {
			t.Fatalf("line %d: want 2 fields: %q", lineNumber, line)
		}
		pinned[strings.TrimSpace(name)] = strings.TrimSpace(path)
	}
	if err := scanner.Err(); err != nil {
		t.Fatalf("scan route pin: %v", err)
	}
	if len(pinned) == 0 {
		t.Fatal("route pin is empty — expected populated routes")
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
	// then the defaults a discovered target carries.
	sites := map[string][]string{
		"probe":          {routeProbe, throughput.Probe, latency.Probe},
		"download":       {routeDownload, throughput.Download},
		"upload":         {routeUpload, throughput.Upload},
		"uploadSession":  {routeUploadSession, throughput.UploadSession},
		"uploadProgress": {routeUploadProgress, throughput.UploadProgress},
		"ping":           {routePing, latency.Ping},
	}
	if len(sites) != len(pinned) {
		t.Errorf("Go declares %d routes; %d are pinned", len(sites), len(pinned))
	}
	for name, paths := range sites {
		want, ok := pinned[name]
		if !ok {
			t.Errorf("%s: declared in Go but not pinned", name)
			continue
		}
		for _, path := range paths {
			if path != want {
				t.Errorf("%s: Go has %q; pinned as %q", name, path, want)
			}
		}
	}
}
