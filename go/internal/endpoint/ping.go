package endpoint

import (
	"time"

	"github.com/zR-JB/graphite-meter/go/internal/transport"
	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

var startMono = time.Now()

// Ping is the WebSocket latency bus (/ws/ping), a stateless echo.
type Ping struct{}

// NewPing builds the latency endpoint.
func NewPing() *Ping { return &Ping{} }

func (p *Ping) ID() string { return "latency" }

// Handle runs the echo loop on the session's message bus: Recv → decode → reply.
func (p *Ping) Handle(s transport.Session) error {
	bus, ok := s.Bus()
	if !ok {
		return transport.ErrUnsupported
	}

	for {
		msg, err := bus.Recv()
		if err != nil {
			return nil // client went away or bus closed, not an error
		}

		f, derr := wire.Decode(msg)
		if derr != nil {
			if de, ok := derr.(*wire.DecodeError); ok {
				if sendErr := bus.Send(wire.Encode(wire.Frame{Op: wire.OpERR, Code: de.Code, Text: de.Text})); sendErr != nil {
					return nil
				}
			}
			continue
		}

		switch f.Op {
		case wire.OpPING:
			pong := wire.Frame{Op: wire.OpPONG, ID: f.ID, Nanos: uint64(time.Since(startMono).Nanoseconds())} //nosec G115
			if err := bus.Send(wire.Encode(pong)); err != nil {
				return nil // conn gone mid-reply, nothing to report
			}
		case wire.OpHI:
			if err := bus.Send(wire.Encode(wire.Frame{Op: wire.OpREADY})); err != nil {
				return nil
			}
		case wire.OpBYE:
			return nil // graceful client-initiated close
		default:
		}
	}
}
