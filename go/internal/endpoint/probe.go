package endpoint

import (
	"encoding/json/v2"
	"net/http"
	"net/netip"

	"github.com/zR-JB/graphite-meter/go/internal/transport"
	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

// LoadFunc reports the server's measurement occupancy: active wrapped handlers and the configured ceiling.
type LoadFunc func() (active, max int)

// Probe returns evidence for the actual selected connection.
type Probe struct {
	trusted       []netip.Prefix
	bootstrapPort string
	load          LoadFunc
}

// NewProbe sets bootstrapPort only on the H3 TCP bootstrap listener.
func NewProbe(trusted []netip.Prefix, bootstrapPort string, load LoadFunc) *Probe {
	return &Probe{trusted: trusted, bootstrapPort: bootstrapPort, load: load}
}

func (p *Probe) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	protocol := httpProtocol(r)
	if p.bootstrapPort != "" && r.ProtoMajor == 1 {
		w.Header().Set("Alt-Svc", `h3=":`+p.bootstrapPort+`"`)
		w.Header().Set("Connection", "close")
	}
	client := transport.ResolveClientAddress(r, p.trusted)
	noStoreJSON(w)
	probe := wire.Probe{
		ClientIP: client.Addr.String(), ClientIPVersion: client.Version,
		ClientIPSource: string(client.Source), ProtocolNegotiated: protocol,
	}
	if p.load != nil {
		active, max := p.load()
		probe.Load = &wire.ProbeLoad{Active: active, Max: max}
	}
	_ = json.MarshalWrite(w, probe)
}

// httpProtocol names the HTTP wire protocol the request actually used.
func httpProtocol(r *http.Request) string {
	switch r.ProtoMajor {
	case 3:
		return "h3"
	case 2:
		return "h2"
	default:
		return "http/1.1"
	}
}
