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

func (p *Preflight) ID() string                 { return "preflight" }
func (p *Preflight) Capabilities() Capabilities { return Capabilities{HTTP: true} }
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

// ConnectOrigins is the set of distinct cross-origin measurement targets this
// server advertises to a browser on the given host, for the authenticated
// CSP's connect-src. It is derived from the very targets /preflight returns, so
// the policy can never omit an origin the client is told to use. The UI's own
// origin ("." / self) is excluded — connect-src 'self' already covers it.
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
	}
	return out
}

// buildForHost assembles the advertised targets for host. Native listeners come
// first, then the configured public origins; both add-helpers deduplicate by
// origin so the same endpoint reached two ways is advertised once. A throughput
// origin claimed under two different protocols becomes "negotiated" — the client
// cannot know in advance which one a proxy in front of it will select.
func (p *Preflight) buildForHost(host string) wire.Preflight {
	throughput := make([]wire.ThroughputTarget, 0)
	latency := make([]wire.LatencyTarget, 0)
	addThroughput := func(base, protocol string) {
		base = strings.TrimRight(base, "/")
		for i := range throughput {
			if origin.Equal(throughput[i].Origin, base) {
				if throughput[i].Protocol != protocol {
					throughput[i].Protocol = "negotiated"
				}
				return
			}
		}
		throughput = append(throughput, wire.ThroughputTarget{ID: base, Origin: base, Transport: "fetch-stream", Protocol: protocol, TLS: strings.HasPrefix(base, "https://"), Routes: wire.DefaultThroughputRoutes()})
	}
	addLatency := func(base string) {
		base = strings.TrimRight(base, "/")
		for _, e := range latency {
			if origin.Equal(e.Origin, base) {
				return
			}
		}
		latency = append(latency, wire.LatencyTarget{ID: base, Origin: base, Transport: "websocket", Protocol: "http1", TLS: strings.HasPrefix(base, "https://"), Routes: wire.DefaultLatencyRoutes()})
	}
	native := []struct {
		name, public, scheme, addr, protocol string
		latency                              bool
	}{
		{config.NativeH1Clear, p.cfg.NativePublic.H1, "http", p.cfg.Native.H1, "http1", true},
		{config.NativeH1TLS, p.cfg.NativePublic.H1TLS, "https", p.cfg.Native.H1TLS, "http1", true},
		{config.NativeH2, p.cfg.NativePublic.H2, "https", p.cfg.Native.H2, "http2", false},
		{config.NativeH3, p.cfg.NativePublic.H3, "https", p.cfg.Native.H3, "http3", false},
	}
	for _, e := range native {
		if p.cfg.NativeAdvertised(e.name) {
			base := nativeOrigin(e.public, e.scheme, host, e.addr)
			addThroughput(base, e.protocol)
			if e.latency {
				addLatency(base)
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
