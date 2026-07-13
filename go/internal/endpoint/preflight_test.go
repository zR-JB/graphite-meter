package endpoint

import (
	"net/http"
	"net/http/httptest"
	"testing"

	"github.com/zR-JB/graphite-meter/go/internal/config"
)

func TestPreflightAdvertisesConfiguredTargets(t *testing.T) {
	cfg := config.Default()
	cfg.EnableH2, cfg.EnableH3 = true, true
	cfg.PublicH1Origin = "http://meter.example:8765"
	cfg.PublicH2Origin = "https://meter.example:8443"
	cfg.PublicH3Origin = "https://meter.example:8444"
	req := httptest.NewRequest(http.MethodGet, "http://discovery.example:8765/preflight", nil)
	pf := NewPreflight(&cfg).build(req)
	if pf.Capabilities.Targets.HTTP1.Origin != cfg.PublicH1Origin {
		t.Fatal("wrong h1 target")
	}
	if pf.Capabilities.Targets.HTTP2.Origin != cfg.PublicH2Origin {
		t.Fatal("wrong h2 target")
	}
	if pf.Capabilities.Targets.HTTP3.Origin != cfg.PublicH3Origin {
		t.Fatal("wrong h3 target")
	}
	if pf.Capabilities.Targets.HTTP3.Routes.WebTransport != nil {
		t.Fatal("webtransport must not be advertised")
	}
}

func TestPreflightOmitsDisabledTargets(t *testing.T) {
	cfg := config.Default()
	pf := NewPreflight(&cfg).build(httptest.NewRequest(http.MethodGet, "http://speed.example:8765/preflight", nil))
	if pf.Capabilities.Targets.HTTP2 != nil || pf.Capabilities.Targets.HTTP3 != nil {
		t.Fatal("disabled TLS targets advertised")
	}
	if got := pf.Capabilities.Targets.HTTP1.Origin; got != "http://speed.example:8765" {
		t.Fatalf("h1 = %q", got)
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
