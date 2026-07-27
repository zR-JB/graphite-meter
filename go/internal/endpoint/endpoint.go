// Package endpoint holds the pluggable measurement modules and the registry
// that mounts them. Each Endpoint's logic is written once against the
// transport.Session abstraction and runs over whichever transport the registry
// hands it.
package endpoint

import "github.com/zR-JB/graphite-meter/go/internal/transport"

// Endpoint is one measurement module (preflight, download, upload, latency).
type Endpoint interface {
	ID() string
	Handle(s transport.Session) error
}
