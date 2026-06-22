package endpoint

import (
	"encoding/json"
	"net"
	"net/http"
	"strconv"

	"github.com/zR-JB/graphite-meter/server/internal/config"
	"github.com/zR-JB/graphite-meter/server/internal/transport"
	"github.com/zR-JB/graphite-meter/server/internal/wire"
)

// Preflight serves GET /preflight: server identity, advertised origins, and
// per-stage capability flags. Request/response JSON (no wire protocol). It also
// mints the per-call upload-session id (store.Mint) that correlates the upload's
// POST lanes with its /ws/upload progress socket.
type Preflight struct {
	cfg   *config.Config
	store *UploadStore
}

// NewPreflight builds the preflight endpoint bound to cfg. store mints the upload
// id surfaced in the response; it may be nil (then no uploadId is issued and the
// client falls back to its own client-side upload count).
func NewPreflight(cfg *config.Config, store *UploadStore) *Preflight {
	return &Preflight{cfg: cfg, store: store}
}

func (p *Preflight) ID() string                  { return "preflight" }
func (p *Preflight) Capabilities() Capabilities  { return Capabilities{HTTP: true} }

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

	// Mint a per-call upload-session id so the upload's POST lanes and its
	// /ws/upload progress socket (separate connections) share one server-side
	// drained-byte count. Empty when no store is wired (client then self-counts).
	var uploadID string
	if p.store != nil {
		uploadID = p.store.Mint()
	}

	// h1 origin: configured public origin, else derived from how the client
	// reached us.
	h1 := cfg.PublicH1Origin
	if h1 == "" {
		h1 = "http://" + r.Host
	}
	origins := wire.Origins{H1: &h1}
	if cfg.PublicTLSOrigin != "" {
		v := cfg.PublicTLSOrigin
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
		UploadID:           uploadID,
		Capabilities: wire.Capabilities{
			Origins: origins,
			Transports: wire.Transports{
				// Honest per stage: Stage 2–3 light up the xhr-stream download/
				// upload; Stage 4 adds the WebSocket latency bus (/ws/ping).
				// WebTransport lands in Stage 5.
				XHRStream:    true,
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
