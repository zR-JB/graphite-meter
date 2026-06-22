package endpoint

import (
	"context"
	"io"
	"testing"
	"time"

	"github.com/zR-JB/graphite-meter/server/internal/transport"
	"github.com/zR-JB/graphite-meter/server/internal/wire"
)

/* ---- test doubles ---- */

// fakeBus is an in-memory transport.MessageBus: `in` carries client→server frames,
// `out` records server→client frames, and closing `closed` simulates the socket
// dropping (Recv then faults, like the real conn close).
type fakeBus struct {
	in     chan string
	out    chan string
	closed chan struct{}
}

func newFakeBus() *fakeBus {
	return &fakeBus{in: make(chan string, 8), out: make(chan string, 256), closed: make(chan struct{})}
}

func (b *fakeBus) Recv() (string, error) {
	select {
	case m, ok := <-b.in:
		if !ok {
			return "", io.EOF
		}
		return m, nil
	case <-b.closed:
		return "", io.EOF
	}
}

func (b *fakeBus) Send(m string) error { b.out <- m; return nil }
func (b *fakeBus) Reliable() bool      { return true }

// busSession is a Session whose Bus() yields a fakeBus (shadowing fakeSession's
// nil stub). query carries the ?id=.
type busSession struct {
	*fakeSession
	bus transport.MessageBus
}

func (s *busSession) Bus() (transport.MessageBus, bool) { return s.bus, true }

// readFrame drains out until a frame matches, or fails on timeout.
func readFrame(t *testing.T, b *fakeBus, match func(wire.Frame) bool, what string) wire.Frame {
	t.Helper()
	deadline := time.After(2 * time.Second)
	for {
		select {
		case raw := <-b.out:
			f, err := wire.Decode(raw)
			if err != nil {
				t.Fatalf("server sent undecodable frame %q: %v", raw, err)
			}
			if match(f) {
				return f
			}
		case <-deadline:
			t.Fatalf("timed out waiting for %s", what)
		}
	}
}

// waitAgg polls until the handler's getOrCreate has run.
func waitAgg(t *testing.T, s *UploadStore, id string) *uploadAgg {
	t.Helper()
	deadline := time.After(time.Second)
	for {
		if a, ok := s.get(id); ok {
			return a
		}
		select {
		case <-deadline:
			t.Fatal("aggregate was never created")
		default:
			time.Sleep(time.Millisecond)
		}
	}
}

/* ---- tests ---- */

// TestUploadProgressFlow drives the full happy path: HI→READY, ticked
// BYTES_RECEIVED tracking the aggregate, BYE→one UPLOAD_COMPLETE with the final
// total, then the aggregate is released and Handle returns.
func TestUploadProgressFlow(t *testing.T) {
	store := NewUploadStore()
	id := store.Mint()
	bus := newFakeBus()
	defer close(bus.closed) // release the recv pump after Handle returns
	sess := &busSession{fakeSession: &fakeSession{ctx: context.Background(), query: "id=" + id}, bus: bus}

	handleErr := make(chan error, 1)
	go func() { handleErr <- NewUploadProgress(store).Handle(sess) }()

	// The WS created the aggregate on first touch; simulate POST lanes draining
	// bytes into it.
	agg := waitAgg(t, store, id)
	agg.bytes.Store(1500)

	// Warmup hello is acknowledged.
	bus.in <- wire.Encode(wire.Frame{Op: wire.OpHI, Proto: "ws"})
	readFrame(t, bus, func(f wire.Frame) bool { return f.Op == wire.OpREADY }, "READY")

	// A tick reports the current server count.
	readFrame(t, bus, func(f wire.Frame) bool {
		return f.Op == wire.OpBytesReceived && f.N == 1500
	}, "BYTES_RECEIVED,1500")

	// More bytes arrive, then the client finishes its lanes and sends BYE.
	agg.bytes.Store(4096)
	bus.in <- wire.Encode(wire.Frame{Op: wire.OpBYE})

	final := readFrame(t, bus, func(f wire.Frame) bool { return f.Op == wire.OpUploadComplete }, "UPLOAD_COMPLETE")
	if final.N != 4096 {
		t.Errorf("UPLOAD_COMPLETE N = %d, want 4096", final.N)
	}

	select {
	case err := <-handleErr:
		if err != nil {
			t.Errorf("Handle returned %v, want nil", err)
		}
	case <-time.After(2 * time.Second):
		t.Fatal("Handle did not return after BYE")
	}
	if _, ok := store.get(id); ok {
		t.Error("aggregate not released after BYE")
	}
}

// TestUploadProgressClientCloseReturns checks a dropped socket (Recv faults) ends
// the handler cleanly — the fast path that bounds the recv pump on a normal close.
func TestUploadProgressClientCloseReturns(t *testing.T) {
	store := NewUploadStore()
	id := store.Mint()
	bus := newFakeBus()
	sess := &busSession{fakeSession: &fakeSession{ctx: context.Background(), query: "id=" + id}, bus: bus}

	handleErr := make(chan error, 1)
	go func() { handleErr <- NewUploadProgress(store).Handle(sess) }()
	waitAgg(t, store, id)

	close(bus.closed) // simulate the connection dropping

	select {
	case err := <-handleErr:
		if err != nil {
			t.Errorf("Handle returned %v, want nil", err)
		}
	case <-time.After(2 * time.Second):
		t.Fatal("Handle did not return after the socket dropped")
	}
}

// TestUploadProgressUnknownID checks an unissued id gets an ERR and the handler
// closes (the client then falls back to its own client-side count).
func TestUploadProgressUnknownID(t *testing.T) {
	store := NewUploadStore()
	bus := newFakeBus()
	defer close(bus.closed)
	sess := &busSession{fakeSession: &fakeSession{ctx: context.Background(), query: "id=forged"}, bus: bus}

	handleErr := make(chan error, 1)
	go func() { handleErr <- NewUploadProgress(store).Handle(sess) }()

	readFrame(t, bus, func(f wire.Frame) bool { return f.Op == wire.OpERR }, "ERR for unknown id")
	select {
	case err := <-handleErr:
		if err != nil {
			t.Errorf("Handle returned %v, want nil", err)
		}
	case <-time.After(2 * time.Second):
		t.Fatal("Handle did not return for an unknown id")
	}
	if store.live.Load() != 0 {
		t.Errorf("live = %d for a forged id, want 0", store.live.Load())
	}
}
