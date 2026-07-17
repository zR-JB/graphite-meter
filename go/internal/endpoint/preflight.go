package endpoint

import (
	"crypto/rand"
	"encoding/hex"
	"encoding/json"
	"net"
	"net/http"
	"net/url"
	"strconv"
	"strings"

	"github.com/zR-JB/graphite-meter/go/internal/config"
	"github.com/zR-JB/graphite-meter/go/internal/transport"
	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

// Preflight serves logical-server discovery. It deliberately ignores the
// request protocol; the selected target's /probe reports path evidence.
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
	host, port := hostPort(r)
	throughput := []wire.ThroughputTarget{
		throughputTarget("http1-clear", origin(p.cfg.PublicH1Origin, "http", host, p.cfg.H1Addr), "http1", false),
	}
	latency := []wire.LatencyTarget{
		latencyTarget("ws-http1-clear", throughput[0].Origin, false),
	}
	if p.cfg.EnableH1TLS || p.cfg.PublicH1TLSOrigin != "" {
		o := origin(p.cfg.PublicH1TLSOrigin, "https", host, p.cfg.H1TLSAddr)
		throughput = append(throughput, throughputTarget("http1-tls", o, "http1", true))
		latency = append(latency, latencyTarget("ws-http1-tls", o, true))
	}
	if p.cfg.EnableH2 || p.cfg.PublicH2Origin != "" {
		o := origin(p.cfg.PublicH2Origin, "https", host, p.cfg.H2Addr)
		throughput = append(throughput, throughputTarget("http2", o, "http2", true))
	}
	if p.cfg.EnableH3 || p.cfg.PublicH3Origin != "" {
		o := origin(p.cfg.PublicH3Origin, "https", host, p.cfg.H3Addr)
		throughput = append(throughput, throughputTarget("http3", o, "http3", true))
	}
	return wire.Preflight{
		Server:        wire.ServerInfo{Name: p.cfg.ServerName, Host: host, Port: port, Location: p.cfg.ServerLocation},
		EngineVersion: p.cfg.EngineVersion,
		Generation:    p.generation,
		Capabilities:  wire.Capabilities{ThroughputTargets: throughput, LatencyTargets: latency},
	}
}

func throughputTarget(id, origin, protocol string, tls bool) wire.ThroughputTarget {
	return wire.ThroughputTarget{ID: id, Origin: strings.TrimRight(origin, "/"), Transport: "fetch-stream", Protocol: protocol, TLS: tls, Routes: wire.DefaultThroughputRoutes()}
}

func latencyTarget(id, origin string, tls bool) wire.LatencyTarget {
	return wire.LatencyTarget{ID: id, Origin: strings.TrimRight(origin, "/"), Transport: "websocket", Protocol: "http1", TLS: tls, Routes: wire.DefaultLatencyRoutes()}
}

func origin(public, scheme, host, addr string) string {
	if public != "" {
		return public
	}
	_, port, err := net.SplitHostPort(addr)
	if err != nil || port == "" {
		return scheme + "://" + host
	}
	return (&url.URL{Scheme: scheme, Host: net.JoinHostPort(host, port)}).String()
}

func hostPort(r *http.Request) (string, int) {
	host, portStr, err := net.SplitHostPort(r.Host)
	if err != nil {
		return r.Host, map[bool]int{true: 443, false: 80}[r.TLS != nil]
	}
	port, err := strconv.Atoi(portStr)
	if err != nil {
		return host, 0
	}
	return host, port
}
