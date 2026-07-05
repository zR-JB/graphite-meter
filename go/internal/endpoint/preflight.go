package endpoint

import (
	"encoding/json"
	"net"
	"net/http"
	"strconv"
	"strings"

	"github.com/zR-JB/graphite-meter/go/internal/config"
	"github.com/zR-JB/graphite-meter/go/internal/transport"
	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

// Preflight serves GET /preflight: server identity, advertised origins, and
// per-stage capability flags. Request/response JSON (no wire protocol). Upload
// correlation tokens are minted later by /upload/session during upload warmup.
// The web client identifies itself via query params (?client=web&
// client_version=<semver>+<label>) — read them here when version-gated
// feature/compat decisions are needed.
type Preflight struct {
	cfg *config.Config
}

// NewPreflight builds the preflight endpoint bound to cfg.
func NewPreflight(cfg *config.Config) *Preflight {
	return &Preflight{cfg: cfg}
}

func (p *Preflight) ID() string                 { return "preflight" }
func (p *Preflight) Capabilities() Capabilities { return Capabilities{HTTP: true} }

func (p *Preflight) Handle(s transport.Session) error {
	w, r, ok := s.HTTP()
	if !ok {
		return transport.ErrUnsupported
	}
	body := p.build(s, r)
	w.Header().Set("Content-Type", "application/json")
	w.Header().Set("Cache-Control", "no-store")
	enc := json.NewEncoder(w)
	enc.SetEscapeHTML(false)
	return enc.Encode(body)
}

func (p *Preflight) build(s transport.Session, r *http.Request) wire.Preflight {
	cfg := p.cfg

	host, port := hostPort(r)

	// h1 origin: configured public origin, else derived from how the client
	// reached us. Always cleartext — see requestIsTLS for the encrypted case.
	h1 := cfg.PublicH1Origin
	if h1 == "" {
		h1 = "http://" + r.Host
	}
	origins := wire.Origins{H1: &h1}
	if cfg.PublicTLSOrigin != "" {
		v := cfg.PublicTLSOrigin
		origins.TLS = &v
	} else if requestIsTLS(r) {
		// A reverse proxy terminated TLS in front of us (or we're somehow
		// reached directly over TLS): derive the encrypted origin the same
		// way h1 is derived, so the client has a real wss(-mappable) origin
		// to prefer instead of falling back to h1's hardcoded http://.
		v := "https://" + r.Host
		origins.TLS = &v
	}
	if cfg.PublicH3Origin != "" {
		v := cfg.PublicH3Origin
		origins.H3 = &v
	}

	return wire.Preflight{
		ClientIP: s.ClientIP(),
		Server: wire.ServerInfo{
			Name:     cfg.ServerName,
			Host:     host,
			Port:     port,
			Location: cfg.ServerLocation,
		},
		PreTestPingMs:      0, // no ping endpoint yet (Stage 4)
		EngineVersion:      cfg.EngineVersion,
		ProtocolNegotiated: string(s.Proto()),
		Capabilities: wire.Capabilities{
			Origins: origins,
			Transports: wire.Transports{
				// Honest per stage: Stage 2–3 light up the fetch-stream download/
				// upload; Stage 4 adds the WebSocket latency bus (/ws/ping).
				// WebTransport lands in Stage 5.
				FetchStream:  true,
				WebSocket:    true,
				WebTransport: false,
			},
			Endpoints: wire.DefaultEndpoints(),
		},
	}
}

// hostPort splits the request Host into a hostname and port, defaulting the
// port to 80 (http) when absent.
func hostPort(r *http.Request) (string, int) {
	host, portStr, err := net.SplitHostPort(r.Host)
	if err != nil {
		// No port in Host.
		return r.Host, 80
	}
	port, err := strconv.Atoi(portStr)
	if err != nil {
		return host, 80
	}
	return host, port
}

// requestIsTLS reports whether this request reached us encrypted: directly
// (r.TLS set) or via a reverse proxy that terminated TLS and says so through
// the de-facto standard X-Forwarded-Proto header (set by nginx/Caddy/Traefik
// by default). Only the first hop is read — this server is meant to sit
// directly behind the terminating proxy, not several hops deep.
func requestIsTLS(r *http.Request) bool {
	if r.TLS != nil {
		return true
	}
	proto := r.Header.Get("X-Forwarded-Proto")
	if i := strings.IndexByte(proto, ','); i >= 0 {
		proto = proto[:i]
	}
	return strings.TrimSpace(proto) == "https"
}
