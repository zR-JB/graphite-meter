package endpoint

import (
	"context"
	"time"

	"github.com/zR-JB/graphite-meter/go/internal/transport"
	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

// Ping echoes probes with their application handling duration.
type Ping struct{}

func NewPing() *Ping { return &Ping{} }

// HandleMessages runs the stateless echo loop; the adapter owns the channel lifetime.
func (p *Ping) HandleMessages(_ context.Context, bus transport.MessageBus) error {
	for {
		message, err := bus.Recv()
		if err != nil {
			return nil
		}
		// The interval excludes adapter receive work and all queues before it returns.
		receivedAt := time.Now()
		id, err := wire.DecodePing(message)
		if err != nil {
			continue
		}
		handling := uint64(time.Since(receivedAt).Nanoseconds()) //nosec G115 -- monotonic elapsed duration
		if err := bus.Send(wire.EncodePong(id, handling)); err != nil {
			return nil
		}
	}
}
