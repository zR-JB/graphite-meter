package endpoint

import (
	"time"

	"github.com/zR-JB/graphite-meter/go/internal/transport"
	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

// UploadProgress is the WebSocket upload-progress bus (/ws/upload): for the
// test named by ?id= it pushes the running server-measured drained byte count
// (aggregated across that test's POST /upload lanes via the shared
// UploadStore) every uploadProgressTick as BYTES_RECEIVED,<n>, and emits
// exactly one UPLOAD_COMPLETE,<n> on the client's BYE. BYE is the sole
// authoritative finalizer — an idle/posts==0 auto-finalize would truncate the
// total during the gaps between repeated POSTs.
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
	// After BYE, allow cancelled POST handlers to finish draining/closing before
	// taking the authoritative final snapshot.
	uploadFinalizeGrace = time.Second
)

func waitForUploadPosts(agg *uploadAgg) {
	deadline := time.Now().Add(uploadFinalizeGrace)
	for agg.posts.Load() > 0 && time.Now().Before(deadline) {
		time.Sleep(time.Millisecond)
	}
}

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
			// N and TIME describe the same instant (client divides Δn/Δtime).
			// Load bytes before time so a race can only under-report, never
			// over-report the rate.
			n := uint64(agg.bytes.Load())
			elapsed := uint64(agg.elapsedNanos(monoNanos()))
			if bus.Send(wire.Encode(wire.Frame{Op: wire.OpBytesReceived, N: n, Nanos: elapsed})) != nil {
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
				// Sole authoritative finalizer, sent once every POST lane has
				// stopped. Emit the final total exactly once, then release state.
				if agg.done.CompareAndSwap(false, true) {
					waitForUploadPosts(agg)
					n := uint64(agg.bytes.Load())
					_ = bus.Send(wire.Encode(wire.Frame{Op: wire.OpUploadComplete, N: n, Nanos: uint64(agg.elapsedNanos(monoNanos()))}))
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
