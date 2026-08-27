// Package endpoint holds the pluggable measurement modules and the registry that mounts them.
package endpoint

import "github.com/zR-JB/graphite-meter/go/internal/transport"

// Endpoint is one measurement module (preflight, download, upload, latency).
type Endpoint interface {
	ID() string
	Handle(s transport.Session) error
}
