package endpoint

import (
	"crypto/rand"
	"encoding/hex"
	"encoding/json"
	"net"
	"net/http"
	"net/url"
	"strings"

	"github.com/zR-JB/graphite-meter/go/internal/config"
	"github.com/zR-JB/graphite-meter/go/internal/origin"
	"github.com/zR-JB/graphite-meter/go/internal/transport"
	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

// Preflight advertises the measurement targets a client should use. generation
// is a per-process random tag: a client that sees it change knows the server
// restarted and its cached target list may be stale.
type Preflight struct {
	cfg        *config.Config
	generation string
}

// NewPreflight builds the preflight endpoint for cfg with a fresh generation
// tag. It panics if the system CSPRNG is unavailable, since every later answer
// would otherwise share an indistinguishable generation.
func NewPreflight(cfg *config.Config) *Preflight {
	var id [16]byte
	if _, err := rand.Read(id[:]); err != nil {
		panic(err)
	}
	return &Preflight{cfg: cfg, generation: hex.EncodeToString(id[:])}
}

func (p *Preflight) ID() string { return "preflight" }
func (p *Preflight) Handle(s transport.Session) error {
	w, r, ok := s.HTTP()
	if !ok {
		return transport.ErrUnsupported
	}
	w.Header().Set("Content-Type", "application/json")
	w.Header().Set("Cache-Control", "no-store")
	return json.NewEncoder(w).Encode(p.build(r))
}

// build derives the bare hostname the client reached us on so native targets can
// be advertised on that same name rather than a configured guess.
func (p *Preflight) build(r *http.Request) wire.Preflight {
	// SplitHostPort fails on a port-less Host; the fallback also unwraps the
	// brackets of a literal IPv6 authority.
	host, _, _ := net.SplitHostPort(r.Host)
	if host == "" {
		host = strings.TrimPrefix(strings.TrimSuffix(r.Host, "]"), "[")
	}
	return p.buildForHost(host)
}

// ConnectOrigins lists the distinct cross-origin measurement targets advertised
// to a browser on host, for the authenticated CSP's connect-src. It reads the
// same targets /preflight returns, so the policy can never omit an origin the
// client is told to use. The UI's own origin (".") is covered by 'self'.
func (p *Preflight) ConnectOrigins(host string) []string {
	pf := p.buildForHost(host)
	seen := map[string]bool{"": true, ".": true}
	out := make([]string, 0)
	add := func(o string) {
		if !seen[o] {
			seen[o] = true
			out = append(out, o)
		}
	}
	for _, t := range pf.Capabilities.ThroughputTargets {
		add(t.Origin)
	}
	for _, t := range pf.Capabilities.LatencyTargets {
		add(t.Origin)
		// Not every engine matches a wss:// URL against an https:// source.
		add(websocketOrigin(t.Origin))
	}
	return out
}

// websocketOrigin maps an http(s) origin to its ws(s) form, "" for anything else.
func websocketOrigin(target string) string {
	switch {
	case strings.HasPrefix(target, "https://"):
		return "wss://" + strings.TrimPrefix(target, "https://")
	case strings.HasPrefix(target, "http://"):
		return "ws://" + strings.TrimPrefix(target, "http://")
	}
	return ""
}

// buildForHost assembles the advertised targets for host: native listeners
// first, then the configured public origins.
func (p *Preflight) buildForHost(host string) wire.Preflight {
	throughput := make([]wire.ThroughputTarget, 0)
	latency := make([]wire.LatencyTarget, 0)
	// Dedupe by origin, so one endpoint reached two ways is advertised once. Two
	// protocols on one origin become "negotiated": a proxy in front picks one.
	addThroughput := func(base, protocol string) {
		base = strings.TrimRight(base, "/")
		for i := range throughput {
			if throughput[i].Transport == wire.TransportFetchStream && origin.Equal(throughput[i].Origin, base) {
				if throughput[i].Protocol != protocol {
					throughput[i].Protocol = "negotiated"
				}
				return
			}
		}
		throughput = append(throughput, wire.ThroughputTarget{ID: base, Origin: base, Transport: wire.TransportFetchStream, Protocol: protocol, TLS: strings.HasPrefix(base, "https://"), Routes: wire.DefaultThroughputRoutes()})
	}
	addLatency := func(base string) {
		base = strings.TrimRight(base, "/")
		for _, e := range latency {
			if e.Transport == wire.TransportWebSocket && origin.Equal(e.Origin, base) {
				return
			}
		}
		latency = append(latency, wire.LatencyTarget{ID: base, Origin: base, Transport: wire.TransportWebSocket, Protocol: "http1", TLS: strings.HasPrefix(base, "https://"), Routes: wire.DefaultLatencyRoutes()})
	}
	// WebTransport is the same HTTP/3 origin reached over QUIC sessions. The
	// stream and datagram modes are separate advertised paths on it.
	addWebTransport := func(base string) {
		base = strings.TrimRight(base, "/")
		throughput = append(throughput, wire.ThroughputTarget{ID: base, Origin: base, Transport: wire.TransportWebTransport, Protocol: "http3", TLS: true, Routes: wire.DefaultThroughputRoutes()})
		throughput = append(throughput, wire.ThroughputTarget{ID: base, Origin: base, Transport: wire.TransportWebTransportDatagram, Protocol: "http3", TLS: true, Routes: wire.DefaultThroughputRoutes()})
		latency = append(latency, wire.LatencyTarget{ID: base, Origin: base, Transport: wire.TransportWebTransport, Protocol: "http3", TLS: true, Routes: wire.DefaultLatencyRoutes()})
	}
	native := []struct {
		name, public, scheme, addr, protocol string
		latency, webTransport                bool
	}{
		{config.NativeH1Clear, p.cfg.NativePublic.H1, "http", p.cfg.Native.H1, "http1", true, false},
		{config.NativeH1TLS, p.cfg.NativePublic.H1TLS, "https", p.cfg.Native.H1TLS, "http1", true, false},
		{config.NativeH2, p.cfg.NativePublic.H2, "https", p.cfg.Native.H2, "http2", false, false},
		{config.NativeH3, p.cfg.NativePublic.H3, "https", p.cfg.Native.H3, "http3", false, true},
	}
	for _, e := range native {
		if p.cfg.NativeAdvertised(e.name) {
			base := nativeOrigin(e.public, e.scheme, host, e.addr)
			addThroughput(base, e.protocol)
			if e.latency {
				addLatency(base)
			}
			if e.webTransport {
				addWebTransport(base)
			}
		}
	}
	for _, base := range p.cfg.Public.Both {
		addThroughput(publicBase(base), "negotiated")
		addLatency(publicBase(base))
	}
	for _, base := range p.cfg.Public.Throughput {
		addThroughput(publicBase(base), "negotiated")
	}
	for _, base := range p.cfg.Public.Latency {
		addLatency(publicBase(base))
	}
	return wire.Preflight{Server: wire.ServerInfo{Name: p.cfg.ServerName, Location: p.cfg.ServerLocation}, EngineVersion: p.cfg.EngineVersion, Generation: p.generation, Capabilities: wire.Capabilities{ThroughputTargets: throughput, LatencyTargets: latency}}
}

// publicBase maps the config's "self" keyword to the wire's "." placeholder,
// which the client resolves against the origin it loaded the UI from.
func publicBase(configured string) string {
	if configured == "self" {
		return "."
	}
	return configured
}

// nativeOrigin returns the explicitly configured public origin, or synthesises
// one from the request's host and the listener's own port.
func nativeOrigin(public, scheme, host, addr string) string {
	if public != "" {
		return public
	}
	_, port, err := net.SplitHostPort(addr)
	if err != nil || port == "" {
		return scheme + "://" + host
	}
	return (&url.URL{Scheme: scheme, Host: net.JoinHostPort(host, port)}).String()
}
