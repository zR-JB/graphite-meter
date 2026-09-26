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

// A CONNECT authenticates with a minted ticket, so authentication does not hide the WebTransport targets.
func TestPreflightNativeEndpointsAreDeterministic(t *testing.T) {
	cfg := config.Default()
	cfg.Auth.Mode = "password"
	cfg.Native.H1TLS, cfg.Native.H2, cfg.Native.H3 = ":7247", ":7248", ":7249"
	cfg.NativePublic = config.NativeEndpoints{H1: "http://meter.example:7246", H1TLS: "https://meter.example:7247",
		H2: "https://meter.example:7248", H3: "https://meter.example:7249"}
	pf := NewDiscovery(&cfg).preflightFor("internal")
	throughput, latency := pf.Capabilities.ThroughputTargets, pf.Capabilities.LatencyTargets
	if len(throughput) != 6 || len(latency) != 3 {
		t.Fatalf("capabilities = %+v, want 6 throughput and 3 latency targets", pf.Capabilities)
	}
	for i, want := range []string{"http1", "http1", "http2", "http3"} {
		if got := throughput[i].Protocol; got != want {
			t.Fatalf("protocol[%d] = %q, want %q", i, got, want)
		}
	}
	for _, target := range []wire.ThroughputTarget{throughput[4], throughput[5]} {
		if target.Origin != cfg.NativePublic.H3 || !strings.HasPrefix(target.Transport, wire.TransportWebTransport) {
			t.Fatalf("webtransport throughput = %+v, want the HTTP/3 origin", target)
		}
	}
	if latency[2].Transport != wire.TransportWebTransport || latency[2].Origin != cfg.NativePublic.H3 {
		t.Fatalf("webtransport latency = %+v, want the HTTP/3 origin", latency[2])
	}
}

func TestPreflightPublicRoles(t *testing.T) {
	for _, tc := range []struct {
		name                   string
		both, through, latency []string
		wantThrough, wantLat   int
	}{
		{"distinct roles", []string{"self", "https://meter.example"},
			[]string{"https://download.example"}, []string{"https://ping.example"}, 3, 3},
		{"duplicate self", []string{"self"}, []string{"self"}, []string{"self"}, 1, 1},
		{"equivalent default port", []string{"https://meter.example"},
			[]string{"https://meter.example:443"}, []string{"https://meter.example:443"}, 1, 1},
	} {
		t.Run(tc.name, func(t *testing.T) {
			cfg := config.Default()
			cfg.AdvertisedNative = map[string]bool{}
			cfg.Public = config.PublicOrigins{Both: tc.both, Throughput: tc.through, Latency: tc.latency}
			pf := NewDiscovery(&cfg).preflightFor("internal")
			throughput := pf.Capabilities.ThroughputTargets
			if len(throughput) != tc.wantThrough || len(pf.Capabilities.LatencyTargets) != tc.wantLat {
				t.Fatalf("capabilities = %+v", pf.Capabilities)
			}
			if throughput[0].Protocol != "negotiated" {
				t.Fatalf("public throughput = %+v, want a negotiated protocol", throughput[0])
			}
		})
	}
	cfg := config.Default()
	pf := NewDiscovery(&cfg).preflightFor(RequestHost(httptest.NewRequest("GET", "http://[::1]/preflight", nil)))
	if got, want := pf.Capabilities.ThroughputTargets[0].Origin, "http://[::1]:7246"; got != want {
		t.Fatalf("native origin = %q, want %q", got, want)
	}
}

func TestConnectOriginsListCrossOriginTargets(t *testing.T) {
	for _, tc := range []struct {
		both, through, latency, want []string
	}{
		{[]string{"self", "https://meter.example"}, []string{"https://download.example"},
			[]string{"https://ping.example"},
			[]string{"https://meter.example", "wss://meter.example", "https://download.example",
				"https://ping.example", "wss://ping.example"}},
		{[]string{"self"}, nil, nil, nil},
		{nil, nil, []string{"http://plain.example:7246"},
			[]string{"http://plain.example:7246", "ws://plain.example:7246"}},
	} {
		cfg := config.Default()
		cfg.AdvertisedNative = map[string]bool{}
		cfg.Public = config.PublicOrigins{Both: tc.both, Throughput: tc.through, Latency: tc.latency}
		got := NewDiscovery(&cfg).ConnectOrigins("meter.example")
		if !slices.Equal(slices.Sorted(slices.Values(got)), slices.Sorted(slices.Values(tc.want))) {
			t.Fatalf("ConnectOrigins = %v, want %v", got, tc.want)
		}
	}
}

func TestPublicConnectionPolicyKeepsSelfAndDNSSourcesForIPv6Page(t *testing.T) {
	cfg := config.Default()
	cfg.ServerCatalog.Servers = append(cfg.ServerCatalog.Servers, wire.ServerEntry{ID: "remote", Name: "Remote",
		URL: "https://meter.example", AdditionalOrigins: []string{"https://[2001:db8::2]:7248"}})
	policy := NewDiscovery(&cfg).ConnectPolicy(RequestHost(httptest.NewRequest(http.MethodGet, "http://[::1]:7246/",
		nil)))
	if strings.Contains(policy, "[") || !strings.Contains(policy, "connect-src 'self' ") ||
		!strings.Contains(policy, "https://meter.example:*") {
		t.Fatalf("unexpected public connection policy: %s", policy)
	}
}
