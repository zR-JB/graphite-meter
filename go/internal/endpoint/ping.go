package endpoint

import (
	"context"
	"errors"
	"time"

	"github.com/zR-JB/graphite-meter/go/internal/transport"
	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

var startMono = time.Now()

// Ping is the WebSocket latency bus (/ws/ping), a stateless echo.
type Ping struct{}

// NewPing builds the latency endpoint.
func NewPing() *Ping { return &Ping{} }

// HandleMessages runs the echo loop; the adapter owns the channel lifetime.
func (p *Ping) HandleMessages(_ context.Context, bus transport.MessageBus) error {
	timing := false
	for {
		msg, err := bus.Recv()
		if err != nil {
			return nil // client went away or bus closed, not an error
		}

		// This boundary excludes the receive call and all queues before it returns.
		receivedAt := time.Now()
		f, derr := wire.Decode(msg)
		if derr != nil {
			if de, ok := errors.AsType[*wire.DecodeError](derr); ok {
				if sendErr := bus.Send(wire.Encode(wire.Frame{Op: wire.OpERR, Code: de.Code, Text: de.Text})); sendErr != nil {
					return nil
				}
			}
			continue
		}

		switch f.Op {
		case wire.OpPING:
			pong := wire.Frame{Op: wire.OpPONG, ID: f.ID, Nanos: uint64(receivedAt.Sub(startMono).Nanoseconds())} //nosec G115
			if timing {
				pong.HandlingNanos = new(uint64(time.Since(receivedAt).Nanoseconds())) //nosec G115 -- monotonic elapsed duration
			}
			if err := bus.Send(wire.Encode(pong)); err != nil {
				return nil // conn gone mid-reply, nothing to report
			}
		case wire.OpHI:
			timing = f.Timing && (f.Proto == "ws" || f.Proto == "wt")
			if err := bus.Send(wire.Encode(wire.Frame{Op: wire.OpREADY, Timing: timing})); err != nil {
				return nil
			}
		case wire.OpBYE:
			return nil // graceful client-initiated close
		default:
		}
	}
}
