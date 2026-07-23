package endpoint

import (
	"net/http/httptest"
	"testing"

	"github.com/zR-JB/graphite-meter/go/internal/config"
)

func TestPreflightNativeEndpointsAreDeterministic(t *testing.T) {
	cfg := config.Default()
	cfg.Native.H1TLS, cfg.Native.H2, cfg.Native.H3 = ":7247", ":7248", ":7249"
	cfg.NativePublic = config.NativeOrigins{H1: "http://meter.example:7246", H1TLS: "https://meter.example:7247", H2: "https://meter.example:7248", H3: "https://meter.example:7249"}
	pf := NewPreflight(&cfg).build(httptest.NewRequest("GET", "http://internal/preflight", nil))
	if len(pf.Capabilities.ThroughputTargets) != 4 || len(pf.Capabilities.LatencyTargets) != 2 {
		t.Fatalf("capabilities = %+v", pf.Capabilities)
	}
	for i, want := range []string{"http1", "http1", "http2", "http3"} {
		if got := pf.Capabilities.ThroughputTargets[i].Protocol; got != want {
			t.Fatalf("protocol[%d] = %q", i, got)
		}
	}
}

func TestPreflightProxyOnlyAndRoles(t *testing.T) {
	cfg := config.Default()
	cfg.AdvertiseAllNative = false
	cfg.AdvertisedNative = map[string]bool{}
	cfg.Public.Both = []string{"self", "https://meter.example"}
	cfg.Public.Throughput = []string{"https://download.example"}
	cfg.Public.Latency = []string{"https://ping.example"}
	pf := NewPreflight(&cfg).build(httptest.NewRequest("GET", "http://internal/preflight", nil))
	if got := pf.Capabilities.ThroughputTargets[0]; got.Origin != "." || got.Protocol != "negotiated" {
		t.Fatalf("self throughput = %+v", got)
	}
	if len(pf.Capabilities.ThroughputTargets) != 3 || len(pf.Capabilities.LatencyTargets) != 3 {
		t.Fatalf("capabilities = %+v", pf.Capabilities)
	}
}

func TestPreflightMergesDuplicatePublicRoles(t *testing.T) {
	cfg := config.Default()
	cfg.AdvertiseAllNative = false
	cfg.AdvertisedNative = map[string]bool{}
	cfg.Public.Both = []string{"self"}
	cfg.Public.Throughput = []string{"self"}
	cfg.Public.Latency = []string{"self"}
	pf := NewPreflight(&cfg).build(httptest.NewRequest("GET", "http://internal/preflight", nil))
	if len(pf.Capabilities.ThroughputTargets) != 1 || len(pf.Capabilities.LatencyTargets) != 1 {
		t.Fatalf("capabilities = %+v", pf.Capabilities)
	}
}

func TestPreflightMergesEquivalentDefaultPortOrigins(t *testing.T) {
	cfg := config.Default()
	cfg.AdvertiseAllNative = false
	cfg.AdvertisedNative = map[string]bool{}
	cfg.Public.Both = []string{"https://meter.example"}
	cfg.Public.Throughput = []string{"https://meter.example:443"}
	cfg.Public.Latency = []string{"https://meter.example:443"}
	pf := NewPreflight(&cfg).build(httptest.NewRequest("GET", "http://internal/preflight", nil))
	if len(pf.Capabilities.ThroughputTargets) != 1 || len(pf.Capabilities.LatencyTargets) != 1 {
		t.Fatalf("capabilities = %+v", pf.Capabilities)
	}
}

func TestPreflightNativeOriginFromBracketedIPv6Host(t *testing.T) {
	cfg := config.Default()
	req := httptest.NewRequest("GET", "http://[::1]/preflight", nil)
	pf := NewPreflight(&cfg).build(req)
	if got, want := pf.Capabilities.ThroughputTargets[0].Origin, "http://[::1]:7246"; got != want {
		t.Fatalf("native origin = %q, want %q", got, want)
	}
}

func TestConnectOriginsListsCrossOriginTargetsAndSkipsSelf(t *testing.T) {
	cfg := config.Default()
	cfg.AdvertiseAllNative = false
	cfg.AdvertisedNative = map[string]bool{}
	cfg.Public.Both = []string{"self", "https://meter.example"}
	cfg.Public.Throughput = []string{"https://download.example"}
	cfg.Public.Latency = []string{"https://ping.example"}

	got := NewPreflight(&cfg).ConnectOrigins("meter.example")
	want := map[string]bool{
		"https://meter.example":    true,
		"https://download.example": true,
		"https://ping.example":     true,
	}
	if len(got) != len(want) {
		t.Fatalf("ConnectOrigins = %v, want the three cross-origins", got)
	}
	for _, o := range got {
		if o == "." || o == "" {
			t.Fatalf("ConnectOrigins leaked a self placeholder: %v", got)
		}
		if !want[o] {
			t.Fatalf("ConnectOrigins returned unexpected %q: %v", o, got)
		}
	}
}

func TestConnectOriginsEmptyWhenEverythingIsSelf(t *testing.T) {
	cfg := config.Default()
	cfg.AdvertiseAllNative = false
	cfg.AdvertisedNative = map[string]bool{}
	cfg.Public.Both = []string{"self"}
	if got := NewPreflight(&cfg).ConnectOrigins("meter.example"); len(got) != 0 {
		t.Fatalf("ConnectOrigins with only self = %v, want empty", got)
	}
}
