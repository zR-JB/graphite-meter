package endpoint

import (
	"fmt"
	"net/http"
	"net/http/httptest"
	"slices"
	"strings"
	"testing"

	"github.com/zR-JB/graphite-meter/go/internal/config"
	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

// A CONNECT authenticates with a minted ticket, so authentication does not hide the WebTransport targets.
func TestPreflightTargetsAndConnectOrigins(t *testing.T) {
	natives := func(cfg *config.Config) {
		cfg.Auth.Mode = "password"
		cfg.Native.H1TLS, cfg.Native.H2, cfg.Native.H3 = ":7247", ":7248", ":7249"
		cfg.NativePublic = config.NativeEndpoints{H1: "http://meter.example:7246", H1TLS: "https://meter.example:7247",
			H2: "https://meter.example:7248", H3: "https://meter.example:7249"}
	}
	public := func(both, throughput, latency []string) func(*config.Config) {
		return func(cfg *config.Config) {
			cfg.AdvertisedNative = map[string]bool{}
			cfg.Public = config.PublicOrigins{Both: both, Throughput: throughput, Latency: latency}
		}
	}
	for _, tc := range []struct {
		name                         string
		configure                    func(*config.Config)
		host                         string
		throughput, latency, connect []string
	}{
		{"authenticated natives", natives, "internal", []string{
			"http1 fetch-stream http://meter.example:7246", "http1 fetch-stream https://meter.example:7247",
			"http2 fetch-stream https://meter.example:7248", "http3 fetch-stream https://meter.example:7249",
			"http3 webtransport https://meter.example:7249", "http3 webtransport-datagram https://meter.example:7249",
		}, []string{
			"websocket http://meter.example:7246", "websocket https://meter.example:7247",
			"webtransport https://meter.example:7249",
		}, []string{
			"http://meter.example:7246", "https://meter.example:7247", "https://meter.example:7248",
			"https://meter.example:7249", "ws://meter.example:7246", "wss://meter.example:7247",
			"wss://meter.example:7249",
		}},
		{"default native on an IPv6 page", func(*config.Config) {}, "[::1]",
			[]string{"http1 fetch-stream http://[::1]:7246"}, []string{"websocket http://[::1]:7246"},
			[]string{"http://[::1]:7246", "ws://[::1]:7246"}},
		{"distinct roles", public([]string{"self", "https://meter.example"}, []string{"https://download.example"},
			[]string{"https://ping.example"}), "meter.example", []string{
			"negotiated fetch-stream .", "negotiated fetch-stream https://meter.example",
			"negotiated fetch-stream https://download.example",
		}, []string{"websocket .", "websocket https://meter.example", "websocket https://ping.example"}, []string{
			"https://meter.example", "wss://meter.example", "https://download.example", "https://ping.example",
			"wss://ping.example",
		}},
		{"duplicate self", public([]string{"self"}, []string{"self"}, []string{"self"}), "meter.example",
			[]string{"negotiated fetch-stream ."}, []string{"websocket ."}, nil},
		{"equivalent default port", public([]string{"https://meter.example"}, []string{"https://meter.example:443"},
			[]string{"https://meter.example:443"}), "meter.example",
			[]string{"negotiated fetch-stream https://meter.example"}, []string{"websocket https://meter.example"},
			[]string{"https://meter.example", "wss://meter.example"}},
		{"plain latency origin", public(nil, nil, []string{"http://plain.example:7246"}), "meter.example", nil,
			[]string{"websocket http://plain.example:7246"},
			[]string{"http://plain.example:7246", "ws://plain.example:7246"}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			cfg := config.Default()
			tc.configure(&cfg)
			d := NewDiscovery(&cfg)
			host := RequestHost(httptest.NewRequest(http.MethodGet, "http://"+tc.host+"/preflight", nil))
			capabilities := d.preflightFor(host).Capabilities
			var throughput, latency []string
			for _, target := range capabilities.ThroughputTargets {
				throughput = append(throughput, target.Protocol+" "+target.Transport+" "+target.Origin)
			}
			for _, target := range capabilities.LatencyTargets {
				latency = append(latency, target.Transport+" "+target.Origin)
			}
			if !slices.Equal(throughput, tc.throughput) || !slices.Equal(latency, tc.latency) {
				t.Fatalf("targets = %q and %q, want %q and %q", throughput, latency, tc.throughput, tc.latency)
			}
			connect := d.ConnectOrigins(host)
			if !slices.Equal(slices.Sorted(slices.Values(connect)), slices.Sorted(slices.Values(tc.connect))) {
				t.Fatalf("ConnectOrigins = %v, want %v", connect, tc.connect)
			}
		})
	}
}

func TestPublicConnectionPolicyKeepsSelfAndDNSSourcesForIPv6Page(t *testing.T) {
	cfg := config.Default()
	cfg.ServerCatalog.Servers = append(cfg.ServerCatalog.Servers, wire.ServerEntry{ID: "remote", Name: "Remote",
		URL: "https://meter.example", AdditionalOrigins: []string{"https://[2001:db8::2]:7248"}})
	policy := NewDiscovery(&cfg).PagePolicy(RequestHost(httptest.NewRequest(http.MethodGet, "http://[::1]:7246/",
		nil)))
	if strings.Contains(policy, "[") || !strings.Contains(policy, "connect-src 'self' ") ||
		!strings.Contains(policy, "https://meter.example:*") {
		t.Fatalf("unexpected public connection policy: %s", policy)
	}
}

// An invalid request host reaches neither targets nor cache, and a flood cannot displace configured hosts.
func TestDiscoveryReadsAnInvalidHostAsLocalhost(t *testing.T) {
	cfg := config.Default()
	cfg.Public.Both = []string{"https://meter.example"}
	d := NewDiscovery(&cfg)
	for _, host := range []string{"evil;host:7246", "evil_host", "-evil.example", "[fe80::1%25evil]",
		strings.Repeat("evil.", 51) + "example"} {
		for _, serve := range []func(http.ResponseWriter, *http.Request){d.ServePreflight, d.ServeServers} {
			r := httptest.NewRequest(http.MethodGet, "/", nil)
			r.Host = host
			rec := httptest.NewRecorder()
			serve(rec, r)
			if rec.Code != http.StatusOK || strings.Contains(rec.Body.String(), "evil") {
				t.Fatalf("%s: %d %s", host, rec.Code, rec.Body.String())
			}
		}
	}
	configured := d.forHost("meter.example")
	for i := range 2 * maxDiscoveryHosts {
		d.forHost(fmt.Sprint("flood-", i, ".example"))
	}
	if len(d.hosts) > maxDiscoveryHosts || d.forHost("meter.example") != configured {
		t.Fatalf("%d cached hosts, configured host kept = %t", len(d.hosts), d.forHost("meter.example") == configured)
	}
}
