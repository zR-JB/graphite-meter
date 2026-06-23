package endpoint

import (
	"time"

	"github.com/zR-JB/graphite-meter/go/internal/transport"
	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

// startMono is the process-start reference for the monotonic TIME stamp echoed in
// PONG. time.Since reads the monotonic clock, so the value never jumps with wall-
// clock changes. It is diagnostics/skew only — the client measures RTT itself.
var startMono = time.Now()

// Ping is the WebSocket latency bus (/ws/ping). It is a pure stateless echo: for
// each PING,<id> it replies PONG,<id>;TIME,<nanos> with the id copied verbatim and
// zero per-ping state (no map, no allocation, no overflow possible server-side —
// api/wire.md §Ids). RTT is measured entirely client-side; the server only mirrors
// the id and stamps a server-monotonic ns for diagnostics.
type Ping struct{}

// NewPing builds the latency endpoint.
func NewPing() *Ping { return &Ping{} }

func (p *Ping) ID() string                 { return "latency" }
func (p *Ping) Capabilities() Capabilities { return Capabilities{WebSocket: true} }

// Handle runs the echo loop on the session's message bus: Recv → decode → reply.
// A read error (client closed the socket / context cancelled) ends the loop
// quietly — a disconnect is normal, not a server error. A single malformed frame
// is answered with ERR,<code>,<text> and the bus stays up (api/wire.md §Framing).
func (p *Ping) Handle(s transport.Session) error {
	bus, ok := s.Bus()
	if !ok {
		return transport.ErrUnsupported
	}

	for {
		msg, err := bus.Recv()
		if err != nil {
			return nil // client went away / bus closed — not an error
		}

		f, derr := wire.Decode(msg)
		if derr != nil {
			// Non-fatal: echo the rejection and keep serving. Never tear down the
			// bus for one bad frame.
			if de, ok := derr.(*wire.DecodeError); ok {
				if sendErr := bus.Send(wire.Encode(wire.Frame{Op: wire.OpERR, Code: de.Code, Text: de.Text})); sendErr != nil {
					return nil
				}
			}
			continue
		}

		switch f.Op {
		case wire.OpPING:
			pong := wire.Frame{Op: wire.OpPONG, ID: f.ID, Nanos: uint64(time.Since(startMono).Nanoseconds())}
			if err := bus.Send(wire.Encode(pong)); err != nil {
				return nil // conn gone mid-reply — nothing to report
			}
		case wire.OpHI:
			// Optional warmup hello — acknowledge so the client can prime the bus
			// during warmup without polluting stats.
			if err := bus.Send(wire.Encode(wire.Frame{Op: wire.OpREADY})); err != nil {
				return nil
			}
		case wire.OpBYE:
			return nil // graceful client-initiated close
		default:
			// A valid but unexpected opcode on the ping bus (e.g. a server→client
			// frame echoed back): ignore it, per the "ignore and continue" rule.
		}
	}
}
