package goclient

import (
	"testing"

	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

func webTransportCatalog() wire.Preflight {
	fetch := testTransfer("https://meter:7249", "https://meter:7249", "http3", true)
	wt := testTransfer("https://meter:7249", "https://meter:7249", "http3", true)
	wt.Transport = wire.TransportWebTransport
	ws := testChannel("https://meter:7247", "https://meter:7247", true)
	wtPing := testChannel("https://meter:7249", "https://meter:7249", true)
	wtPing.Transport, wtPing.Protocol = wire.TransportWebTransport, "http3"
	return wire.Preflight{Capabilities: wire.Capabilities{
		ThroughputTargets: []wire.ThroughputTarget{fetch, wt},
		LatencyTargets:    []wire.LatencyTarget{ws, wtPing},
	}}
}

// TestAutomaticSelectionPreference pins the order: throughput prefers fetch
// streams, latency prefers the datagram bus, whose loss is real packet loss.
func TestAutomaticSelectionPreference(t *testing.T) {
	pf := webTransportCatalog()
	cfg := Config{BaseURL: "https://meter:7249", ThroughputTarget: "auto", ThroughputTransport: "auto", LatencyTarget: "auto", LatencyTransport: "auto"}

	throughput, err := selectTarget(cfg, pf)
	if err != nil || throughput.Transport != wire.TransportFetchStream {
		t.Fatalf("automatic throughput target = %+v, %v", throughput, err)
	}
	latency, err := selectLatencyTarget(cfg, pf.Capabilities.LatencyTargets)
	if err != nil || latency.Transport != wire.TransportWebTransport {
		t.Fatalf("automatic latency target = %+v, %v", latency, err)
	}

	// An origin advertising only WebTransport remains reachable automatically.
	wtOnly := wire.Preflight{Capabilities: wire.Capabilities{ThroughputTargets: []wire.ThroughputTarget{pf.Capabilities.ThroughputTargets[1]}}}
	fallback, err := selectTarget(cfg, wtOnly)
	if err != nil || fallback.Transport != wire.TransportWebTransport {
		t.Fatalf("fallback throughput target = %+v, %v", fallback, err)
	}
}

// TestExplicitTransportSelectionIsHonoured keeps a named transport from silently
// resolving to another one.
func TestExplicitTransportSelectionIsHonoured(t *testing.T) {
	pf := webTransportCatalog()
	cfg := Config{BaseURL: "https://meter:7249", ThroughputTarget: "auto", ThroughputTransport: wire.TransportFetchStream, LatencyTarget: "auto", LatencyTransport: wire.TransportWebSocket}

	throughput, err := selectTarget(cfg, pf)
	if err != nil || throughput.Transport != wire.TransportFetchStream {
		t.Fatalf("explicit throughput transport = %+v, %v", throughput, err)
	}
	latency, err := selectLatencyTarget(cfg, pf.Capabilities.LatencyTargets)
	if err != nil || latency.Transport != wire.TransportWebSocket {
		t.Fatalf("explicit latency transport = %+v, %v", latency, err)
	}

	cfg.ThroughputTransport = wire.TransportWebTransport
	cfg.LatencyTransport = wire.TransportWebTransport
	bare := wire.Preflight{Capabilities: wire.Capabilities{
		ThroughputTargets: []wire.ThroughputTarget{testTransfer("h1", "http://meter:7246", "http1", false)},
		LatencyTargets:    []wire.LatencyTarget{testChannel("h1", "http://meter:7246", false)},
	}}
	if got, err := selectTarget(cfg, bare); err == nil {
		t.Fatalf("throughput fell back to %+v; want a refusal", got)
	}
	if got, err := selectLatencyTarget(cfg, bare.Capabilities.LatencyTargets); err == nil {
		t.Fatalf("latency fell back to %+v; want a refusal", got)
	}
}

func TestConnectionSummaryNamesWebTransport(t *testing.T) {
	if got, want := connectionSummary(wire.TransportWebTransport, "http3", true), "WebTransport · HTTP/3 · TLS"; got != want {
		t.Fatalf("connectionSummary = %q, want %q", got, want)
	}
}
