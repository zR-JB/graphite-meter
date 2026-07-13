package endpoint

import (
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
type Preflight struct{ cfg *config.Config }

func NewPreflight(cfg *config.Config) *Preflight { return &Preflight{cfg: cfg} }
func (p *Preflight) ID() string                  { return "preflight" }
func (p *Preflight) Capabilities() Capabilities  { return Capabilities{HTTP: true} }

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
	targets := wire.Targets{HTTP1: target(origin(p.cfg.PublicH1Origin, "http", host, p.cfg.H1Addr), true)}
	if p.cfg.EnableH2 || p.cfg.PublicH2Origin != "" {
		targets.HTTP2 = target(origin(p.cfg.PublicH2Origin, "https", host, p.cfg.H2Addr), true)
	}
	if p.cfg.EnableH3 || p.cfg.PublicH3Origin != "" {
		targets.HTTP3 = target(origin(p.cfg.PublicH3Origin, "https", host, p.cfg.H3Addr), true)
	}
	return wire.Preflight{
		Server:        wire.ServerInfo{Name: p.cfg.ServerName, Host: host, Port: port, Location: p.cfg.ServerLocation},
		EngineVersion: p.cfg.EngineVersion,
		Capabilities:  wire.Capabilities{Targets: targets},
	}
}

func target(origin string, ws bool) *wire.Target {
	return &wire.Target{Origin: strings.TrimRight(origin, "/"), Routes: wire.DefaultRoutes(ws)}
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
