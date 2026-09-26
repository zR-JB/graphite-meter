package endpoint

import (
	"net/http"
	"net/http/httptest"
	"slices"
	"strings"
	"testing"

	"github.com/zR-JB/graphite-meter/go/internal/config"
	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

func TestPreflightNativeEndpointsAreDeterministic(t *testing.T) {
	cfg := config.Default()
	cfg.Native.H1TLS, cfg.Native.H2, cfg.Native.H3 = ":7247", ":7248", ":7249"
	cfg.NativePublic = config.NativeOrigins{H1: "http://meter.example:7246", H1TLS: "https://meter.example:7247", H2: "https://meter.example:7248", H3: "https://meter.example:7249"}
	pf := NewDiscovery(&cfg).preflightFor("internal")
	// The preflight includes fetch, WebTransport stream, and WebTransport datagram targets.
	if len(pf.Capabilities.ThroughputTargets) != 6 || len(pf.Capabilities.LatencyTargets) != 3 {
		t.Fatalf("capabilities = %+v, want 6 throughput and 3 latency targets", pf.Capabilities)
	}
	for i, want := range []string{"http1", "http1", "http2", "http3"} {
		if got := pf.Capabilities.ThroughputTargets[i].Protocol; got != want {
			t.Fatalf("protocol[%d] = %q, want %q", i, got, want)
		}
	}
	for i, want := range []string{wire.TransportWebTransport, wire.TransportWebTransportDatagram} {
		wt := pf.Capabilities.ThroughputTargets[4+i]
		if wt.Transport != want || wt.Origin != cfg.NativePublic.H3 {
			t.Fatalf("webtransport throughput[%d] = %+v, want %s on the HTTP/3 origin", i, wt, want)
		}
	}
	wtLatency := pf.Capabilities.LatencyTargets[2]
	if wtLatency.Transport != wire.TransportWebTransport || wtLatency.Origin != cfg.NativePublic.H3 {
		t.Fatalf("webtransport latency = %+v, want the HTTP/3 origin", wtLatency)
	}
}

// A CONNECT authenticates with a minted token, so auth does not hide the WebTransport targets.
func TestPreflightAdvertisesWebTransportUnderAuth(t *testing.T) {
	cfg := config.Default()
	cfg.Native.H3 = ":7249"
	cfg.NativePublic.H3 = "https://meter.example:7249"
	cfg.Auth.Mode = "password"
	pf := NewDiscovery(&cfg).preflightFor("internal")
	throughput, latency := false, false
	for _, target := range pf.Capabilities.ThroughputTargets {
		throughput = throughput || target.Transport == wire.TransportWebTransport
	}
	for _, target := range pf.Capabilities.LatencyTargets {
		latency = latency || target.Transport == wire.TransportWebTransport
	}
	if !throughput || !latency {
		t.Fatalf("WebTransport advertised: throughput=%t latency=%t, want both", throughput, latency)
	}
}

func TestPreflightProxyOnlyAndRoles(t *testing.T) {
	cfg := config.Default()
	cfg.AdvertiseAllNative = false
	cfg.AdvertisedNative = map[string]bool{}
	cfg.Public.Both = []string{"self", "https://meter.example"}
	cfg.Public.Throughput = []string{"https://download.example"}
	cfg.Public.Latency = []string{"https://ping.example"}
	pf := NewDiscovery(&cfg).preflightFor("internal")
	if got := pf.Capabilities.ThroughputTargets[0]; got.Origin != "." || got.Protocol != "negotiated" {
		t.Fatalf("self throughput = %+v, want origin \".\" and protocol \"negotiated\"", got)
	}
	if len(pf.Capabilities.ThroughputTargets) != 3 || len(pf.Capabilities.LatencyTargets) != 3 {
		t.Fatalf("capabilities = %+v, want 3 throughput and 3 latency targets", pf.Capabilities)
	}
}

func TestPreflightMergesDuplicatePublicRoles(t *testing.T) {
	cfg := config.Default()
	cfg.AdvertiseAllNative = false
	cfg.AdvertisedNative = map[string]bool{}
	cfg.Public.Both = []string{"self"}
	cfg.Public.Throughput = []string{"self"}
	cfg.Public.Latency = []string{"self"}
	pf := NewDiscovery(&cfg).preflightFor("internal")
	if len(pf.Capabilities.ThroughputTargets) != 1 || len(pf.Capabilities.LatencyTargets) != 1 {
		t.Fatalf("capabilities = %+v, want 1 throughput and 1 latency target", pf.Capabilities)
	}
}

func TestPreflightMergesEquivalentDefaultPortOrigins(t *testing.T) {
	cfg := config.Default()
	cfg.AdvertiseAllNative = false
	cfg.AdvertisedNative = map[string]bool{}
	cfg.Public.Both = []string{"https://meter.example"}
	cfg.Public.Throughput = []string{"https://meter.example:443"}
	cfg.Public.Latency = []string{"https://meter.example:443"}
	pf := NewDiscovery(&cfg).preflightFor("internal")
	if len(pf.Capabilities.ThroughputTargets) != 1 || len(pf.Capabilities.LatencyTargets) != 1 {
		t.Fatalf("capabilities = %+v, want 1 throughput and 1 latency target", pf.Capabilities)
	}
}

func TestPreflightNativeOriginFromBracketedIPv6Host(t *testing.T) {
	cfg := config.Default()
	req := httptest.NewRequest("GET", "http://[::1]/preflight", nil)
	pf := NewDiscovery(&cfg).preflightFor(RequestHost(req))
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

	got := NewDiscovery(&cfg).ConnectOrigins("meter.example")
	// Latency targets carry a ws(s) form as well: the WebSocket URL scheme.
	want := map[string]bool{
		"https://meter.example":    true,
		"wss://meter.example":      true,
		"https://download.example": true,
		"https://ping.example":     true,
		"wss://ping.example":       true,
	}
	if len(got) != len(want) {
		t.Fatalf("ConnectOrigins = %v, want %d origins", got, len(want))
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
	if got := NewDiscovery(&cfg).ConnectOrigins("meter.example"); len(got) != 0 {
		t.Fatalf("ConnectOrigins with only self = %v, want empty", got)
	}
}

func TestConnectOriginsCarriesWebSocketSchemes(t *testing.T) {
	cfg := config.Default()
	cfg.AdvertiseAllNative = false
	cfg.AdvertisedNative = map[string]bool{}
	cfg.Public.Latency = []string{"https://ping.example", "http://plain.example:7246"}

	got := NewDiscovery(&cfg).ConnectOrigins("meter.example")
	for _, want := range []string{"wss://ping.example", "ws://plain.example:7246"} {
		if !slices.Contains(got, want) {
			t.Fatalf("ConnectOrigins = %v, want it to contain %q", got, want)
		}
	}
}

func TestPublicConnectionPolicyKeepsSelfAndDNSSourcesForIPv6Page(t *testing.T) {
	cfg := config.Default()
	cfg.ServerCatalog = wire.SingletonCatalog()
	cfg.ServerCatalog.Servers = append(cfg.ServerCatalog.Servers, wire.ServerEntry{ID: "remote", Name: "Remote", URL: "https://meter.example", AdditionalOrigins: []string{"https://[2001:db8::2]:7248"}})
	policy := NewDiscovery(&cfg).ConnectPolicy(RequestHost(httptest.NewRequest(http.MethodGet, "http://[::1]:7246/", nil)))
	if strings.Contains(policy, "[") || !strings.Contains(policy, "connect-src 'self' ") || !strings.Contains(policy, "https://meter.example:*") {
		t.Fatalf("unexpected public connection policy: %s", policy)
	}
}
