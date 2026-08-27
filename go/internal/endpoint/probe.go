package endpoint

import (
	"encoding/json/v2"

	"github.com/zR-JB/graphite-meter/go/internal/config"
	"github.com/zR-JB/graphite-meter/go/internal/transport"
	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

// LoadFunc reports the server's measurement occupancy: active wrapped handlers and the configured ceiling.
type LoadFunc func() (active, max int)

// Probe returns evidence for the actual selected connection.
type Probe struct {
	cfg           *config.Config
	bootstrapPort string
	load          LoadFunc
}

// NewProbe builds the probe endpoint. bootstrapPort must be set only on the H3 TCP bootstrap listener.
func NewProbe(cfg *config.Config, bootstrapPort string, load LoadFunc) *Probe {
	return &Probe{cfg: cfg, bootstrapPort: bootstrapPort, load: load}
}

func (p *Probe) ID() string { return "probe" }

func (p *Probe) Handle(s transport.Session) error {
	w, r, ok := s.HTTP()
	if !ok {
		return transport.ErrUnsupported
	}
	if p.bootstrapPort != "" && s.Proto() == transport.ProtoH1 {
		w.Header().Set("Alt-Svc", `h3=":`+p.bootstrapPort+`"`)
		w.Header().Set("Connection", "close")
	}
	client := transport.ResolveClientAddress(r, p.cfg.TrustedProxies)
	w.Header().Set("Content-Type", "application/json")
	w.Header().Set("Cache-Control", "no-store")
	probe := wire.Probe{
		ClientIP: client.Addr.String(), ClientIPVersion: client.Version,
		ClientIPSource: string(client.Source), ProtocolNegotiated: string(s.Proto()),
	}
	if p.load != nil {
		active, max := p.load()
		probe.Load = &wire.ProbeLoad{Active: active, Max: max}
	}
	return json.MarshalWrite(w, probe)
}
