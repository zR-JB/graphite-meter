// Package endpoint holds the pluggable measurement modules and the registry
// that mounts them. Each Endpoint's logic is written once against the
// transport.Session abstraction and runs over whichever transport (HTTP today;
// WebSocket / WebTransport in later stages) the registry hands it.
package endpoint

import "github.com/zR-JB/graphite-meter/server/internal/transport"

// Capabilities declares which transport classes an endpoint can be served over.
// Informational for now; later stages use it to drive mounting.
type Capabilities struct {
	HTTP         bool
	WebSocket    bool
	WebTransport bool
}

// Endpoint is one measurement module (preflight, download, upload, latency).
type Endpoint interface {
	ID() string
	Capabilities() Capabilities
	Handle(s transport.Session) error
}
