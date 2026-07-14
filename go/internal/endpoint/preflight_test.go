package endpoint

import (
	"net/http"
	"net/http/httptest"
	"testing"

	"github.com/zR-JB/graphite-meter/go/internal/config"
)

func TestPreflightAdvertisesConfiguredTargets(t *testing.T) {
	cfg := config.Default()
	cfg.EnableH1TLS, cfg.EnableH2, cfg.EnableH3 = true, true, true
	cfg.PublicH1Origin = "http://meter.example:7246"
	cfg.PublicH1TLSOrigin = "https://meter.example:7247"
	cfg.PublicH2Origin = "https://meter.example:7248"
	cfg.PublicH3Origin = "https://meter.example:7249"
	req := httptest.NewRequest(http.MethodGet, "http://discovery.example:7246/preflight", nil)
	pf := NewPreflight(&cfg).build(req)
	if len(pf.Capabilities.ThroughputTargets) != 4 || len(pf.Capabilities.LatencyTargets) != 2 {
		t.Fatalf("capabilities = %+v", pf.Capabilities)
	}
	want := []struct {
		id, origin, protocol string
		tls                  bool
	}{
		{"http1-clear", cfg.PublicH1Origin, "http1", false},
		{"http1-tls", cfg.PublicH1TLSOrigin, "http1", true},
		{"http2", cfg.PublicH2Origin, "http2", true},
		{"http3", cfg.PublicH3Origin, "http3", true},
	}
	for i, transfer := range pf.Capabilities.ThroughputTargets {
		if transfer.ID != want[i].id || transfer.Origin != want[i].origin || transfer.Transport != "fetch-stream" || transfer.Protocol != want[i].protocol || transfer.TLS != want[i].tls {
			t.Errorf("transfer %d = %+v", i, transfer)
		}
	}
	for i, latency := range pf.Capabilities.LatencyTargets {
		if latency.Transport != "websocket" || latency.Protocol != "http1" || latency.Origin != want[i].origin {
			t.Errorf("latency target %d = %+v", i, latency)
		}
		if latency.ID != []string{"ws-http1-clear", "ws-http1-tls"}[i] {
			t.Errorf("latency target %d id = %q", i, latency.ID)
		}
	}
}

func TestPreflightDerivesEveryDefaultListenerOrigin(t *testing.T) {
	cfg := config.Default()
	cfg.EnableH1TLS, cfg.EnableH2, cfg.EnableH3 = true, true, true
	pf := NewPreflight(&cfg).build(httptest.NewRequest(http.MethodGet, "http://meter.example:7246/preflight", nil))
	want := []string{
		"http://meter.example:7246",
		"https://meter.example:7247",
		"https://meter.example:7248",
		"https://meter.example:7249",
	}
	for i, target := range pf.Capabilities.ThroughputTargets {
		if target.Origin != want[i] {
			t.Errorf("throughput target %q origin = %q, want %q", target.ID, target.Origin, want[i])
		}
	}
	for i, target := range pf.Capabilities.LatencyTargets {
		if target.Origin != want[i] {
			t.Errorf("latency target %q origin = %q, want %q", target.ID, target.Origin, want[i])
		}
	}
}

func TestPreflightOmitsDisabledTargets(t *testing.T) {
	cfg := config.Default()
	pf := NewPreflight(&cfg).build(httptest.NewRequest(http.MethodGet, "http://speed.example:7246/preflight", nil))
	if len(pf.Capabilities.ThroughputTargets) != 1 || len(pf.Capabilities.LatencyTargets) != 1 {
		t.Fatalf("disabled TLS capabilities advertised: %+v", pf.Capabilities)
	}
	if got := pf.Capabilities.ThroughputTargets[0].Origin; got != "http://speed.example:7246" {
		t.Fatalf("h1 = %q", got)
	}
}

func TestPreflightAdvertisesExternalProxyTargetsWithoutNativeTLS(t *testing.T) {
	cfg := config.Default()
	cfg.PublicH1TLSOrigin = "https://h1.example"
	cfg.PublicH2Origin = "https://meter.example"
	cfg.PublicH3Origin = "https://quic.example"
	pf := NewPreflight(&cfg).build(httptest.NewRequest(http.MethodGet, "http://internal:7246/preflight", nil))
	for i, want := range []string{"http://internal:7246", cfg.PublicH1TLSOrigin, cfg.PublicH2Origin, cfg.PublicH3Origin} {
		if got := pf.Capabilities.ThroughputTargets[i].Origin; got != want {
			t.Errorf("external transfer %d = %q, want %q", i, got, want)
		}
	}
}

func TestHostPortDefaultsByTLS(t *testing.T) {
	for _, tc := range []struct {
		url  string
		want int
	}{{"http://speed.example/preflight", 80}, {"https://speed.example/preflight", 443}} {
		req := httptest.NewRequest(http.MethodGet, tc.url, nil)
		_, got := hostPort(req)
		if got != tc.want {
			t.Errorf("%s port = %d", tc.url, got)
		}
	}
}
