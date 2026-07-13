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
	cfg.PublicH1Origin = "http://meter.example:8765"
	cfg.PublicH1TLSOrigin = "https://meter.example:8445"
	cfg.PublicH2Origin = "https://meter.example:8443"
	cfg.PublicH3Origin = "https://meter.example:8444"
	req := httptest.NewRequest(http.MethodGet, "http://discovery.example:8765/preflight", nil)
	pf := NewPreflight(&cfg).build(req)
	if len(pf.Capabilities.Transfers) != 4 || len(pf.Capabilities.Channels) != 4 {
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
	for i, transfer := range pf.Capabilities.Transfers {
		if transfer.ID != want[i].id || transfer.Origin != want[i].origin || transfer.Transport != "fetch-stream" || transfer.Protocol != want[i].protocol || transfer.TLS != want[i].tls {
			t.Errorf("transfer %d = %+v", i, transfer)
		}
		channel := pf.Capabilities.Channels[i]
		if channel.Transport != "websocket" || channel.Protocol != "http1" || channel.Origin != transfer.Origin {
			t.Errorf("channel %d = %+v", i, channel)
		}
	}
}

func TestPreflightOmitsDisabledTargets(t *testing.T) {
	cfg := config.Default()
	pf := NewPreflight(&cfg).build(httptest.NewRequest(http.MethodGet, "http://speed.example:8765/preflight", nil))
	if len(pf.Capabilities.Transfers) != 1 || len(pf.Capabilities.Channels) != 1 {
		t.Fatalf("disabled TLS capabilities advertised: %+v", pf.Capabilities)
	}
	if got := pf.Capabilities.Transfers[0].Origin; got != "http://speed.example:8765" {
		t.Fatalf("h1 = %q", got)
	}
}

func TestPreflightAdvertisesExternalProxyTargetsWithoutNativeTLS(t *testing.T) {
	cfg := config.Default()
	cfg.PublicH1TLSOrigin = "https://h1.example"
	cfg.PublicH2Origin = "https://meter.example"
	cfg.PublicH3Origin = "https://quic.example"
	pf := NewPreflight(&cfg).build(httptest.NewRequest(http.MethodGet, "http://internal:8765/preflight", nil))
	for i, want := range []string{"http://internal:8765", cfg.PublicH1TLSOrigin, cfg.PublicH2Origin, cfg.PublicH3Origin} {
		if got := pf.Capabilities.Transfers[i].Origin; got != want {
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
