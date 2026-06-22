package endpoint

import (
	"time"

	"github.com/zR-JB/graphite-meter/server/internal/transport"
	"github.com/zR-JB/graphite-meter/server/internal/wire"
)

// UploadProgress is the WebSocket upload-progress bus (/ws/upload). It is the
// server→client half of server-authoritative upload (docs/UPLOAD_ARCHITECTURE.md
// §4): for the test named by ?id= it pushes the running SERVER-measured drained
// byte count — aggregated across that test's separate POST /upload lanes via the
// shared UploadStore — every uploadProgressTick as BYTES_RECEIVED,<n>, and emits
// exactly one UPLOAD_COMPLETE,<n> when the client sends BYE (after it has stopped
// all its POST lanes). BYE is the SOLE authoritative finalizer: an idle/posts==0
// auto-finalize would truncate the total during the gaps between repeated POSTs.
type UploadProgress struct {
	store *UploadStore
}

// NewUploadProgress builds the progress bus bound to the shared per-id store.
func NewUploadProgress(store *UploadStore) *UploadProgress {
	return &UploadProgress{store: store}
}

func (e *UploadProgress) ID() string                 { return "upload-progress" }
func (e *UploadProgress) Capabilities() Capabilities { return Capabilities{WebSocket: true} }

const (
	// uploadProgressTick is the server push cadence. 100 ms (not 250) keeps the
	// sample density the client's existing throughput tuning expects and is a bare
	// atomic Load + a tiny Send — genuinely off the critical path.
	uploadProgressTick = 100 * time.Millisecond
	// uploadProgressIdle bounds the handler's goroutines: if no client frame (HI
	// keepalive / BYE) arrives for this long the client has silently vanished, so
	// the handler returns and the conn closes (which faults the parked recv pump).
	// The client keeps the read side warm with a periodic HI keepalive.
	uploadProgressIdle = 10 * time.Second
)

// Handle runs the progress loop for one /ws/upload socket. It resolves the test's
// shared aggregate from ?id= (create-on-first-touch — the WS may open before any
// POST), then pushes BYTES_RECEIVED on a ticker and finalizes on BYE. A recv pump
// runs the blocking bus.Recv off the select; closing the conn on return unblocks
// it (no leaked goroutine). A bad frame is answered with ERR and the bus stays up.
func (e *UploadProgress) Handle(s transport.Session) error {
	bus, ok := s.Bus()
	if !ok {
		return transport.ErrUnsupported
	}

	id := s.Query().Get("id")
	agg, ok := e.store.getOrCreate(id)
	if !ok {
		// Empty / unissued / over-cap id: nothing authoritative to report. Tell the
		// client and close — it falls back to its own client-side upload count.
		_ = bus.Send(wire.Encode(wire.Frame{Op: wire.OpERR, Code: wire.ErrBadArgs, Text: "unknown upload id"}))
		return nil
	}

	// Recv pump: bus.Recv (conn.Read) blocks, so it runs off the select. It exits
	// when Handle returns and the wsAdapter closes the conn (read faults), or when
	// `done` is closed while it is parked forwarding a frame.
	frames := make(chan string)
	done := make(chan struct{})
	recvDone := make(chan struct{})
	defer close(done)
	go func() {
		for {
			msg, err := bus.Recv()
			if err != nil {
				close(recvDone)
				return
			}
			select {
			case frames <- msg:
			case <-done:
				return
			}
		}
	}()

	ticker := time.NewTicker(uploadProgressTick)
	defer ticker.Stop()

	idleNanos := int64(uploadProgressIdle)
	lastRecv := monoNanos() // the open itself counts as activity

	for {
		select {
		case <-recvDone:
			return nil // client closed / connection faulted — a normal end

		case <-ticker.C:
			if monoNanos()-lastRecv > idleNanos {
				return nil // no keepalive for too long — client vanished silently
			}
			agg.lastTouchMono.Store(monoNanos()) // keep the id non-idle for the sweeper
			// N and TIME are sampled together: the client divides Δn by Δtime, so they
			// must describe the same instant. TIME is NOT the wall clock — it is the
			// aggregate's ACTIVE measurement clock (ns bytes were actually flowing,
			// dead zones excluded), the upload twin of the download read-side timing.
			// Load bytes BEFORE active so a racing chunk can only make active outrun
			// bytes (under-report), never the reverse (over-report).
			n := uint64(agg.bytes.Load())
			active := uint64(agg.activeNanos.Load())
			if bus.Send(wire.Encode(wire.Frame{Op: wire.OpBytesReceived, N: n, Nanos: active})) != nil {
				return nil // socket gone mid-send — client is away
			}

		case msg := <-frames:
			lastRecv = monoNanos()
			f, derr := wire.Decode(msg)
			if derr != nil {
				if de, ok := derr.(*wire.DecodeError); ok {
					if bus.Send(wire.Encode(wire.Frame{Op: wire.OpERR, Code: de.Code, Text: de.Text})) != nil {
						return nil
					}
				}
				continue // one bad frame never tears the bus down
			}
			switch f.Op {
			case wire.OpHI:
				// Warmup hello / keepalive — acknowledge so the client can prime and
				// keep the read side warm without polluting the measurement.
				if bus.Send(wire.Encode(wire.Frame{Op: wire.OpREADY})) != nil {
					return nil
				}
			case wire.OpBYE:
				// The sole authoritative finalizer: the client sends BYE only after
				// every POST lane has stopped. Emit the final total exactly once,
				// then release the test's state (TTL would otherwise reap it later).
				if agg.done.CompareAndSwap(false, true) {
					// Final total + final active measurement time, the same pair the live
					// ticks carry — so the client's headline denominator is the server's
					// own active clock, not a span across frame arrivals.
					_ = bus.Send(wire.Encode(wire.Frame{Op: wire.OpUploadComplete, N: uint64(agg.bytes.Load()), Nanos: uint64(agg.activeNanos.Load())}))
				}
				e.store.delete(id)
				return nil
			default:
				// A valid but unexpected opcode (e.g. a server→client frame echoed
				// back): ignore and continue, per the wire "ignore and continue" rule.
			}
		}
	}
}
