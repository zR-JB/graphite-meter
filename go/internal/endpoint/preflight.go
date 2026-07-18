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

type Preflight struct {
	cfg        *config.Config
	generation string
}

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

func (p *Preflight) build(r *http.Request) wire.Preflight {
	host, _, _ := net.SplitHostPort(r.Host)
	if host == "" {
		host = strings.TrimPrefix(strings.TrimSuffix(r.Host, "]"), "[")
	}
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

func publicBase(origin string) string {
	if origin == "self" {
		return "."
	}
	return origin
}

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
