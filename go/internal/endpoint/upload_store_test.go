package endpoint

import (
	"strconv"
	"sync"
	"testing"
	"time"
)

// TestUploadStoreRejectsUnissuedID guards the abuse defence: an id the server never
// minted at /upload/session cannot create an aggregate.
func TestUploadStoreRejectsUnissuedID(t *testing.T) {
	s := NewUploadStore()
	if agg, ok := s.getOrCreate("never-minted"); ok || agg != nil {
		t.Fatalf("getOrCreate on an unissued id = (%v, %v), want (nil, false)", agg, ok)
	}
	if s.live.Load() != 0 {
		t.Errorf("live = %d after a rejected create, want 0", s.live.Load())
	}
}

// TestUploadStoreCreateIsIdempotent checks that POST and WS (or repeated lanes)
// carrying the same minted id all resolve to ONE aggregate, and that counting works.
func TestUploadStoreCreateIsIdempotent(t *testing.T) {
	s := NewUploadStore()
	s.markIssued("test-1")

	a, ok := s.getOrCreate("test-1")
	if !ok || a == nil {
		t.Fatalf("first getOrCreate failed: ok=%v", ok)
	}
	b, ok := s.getOrCreate("test-1")
	if !ok || b != a {
		t.Fatalf("second getOrCreate returned a different aggregate (%p vs %p)", b, a)
	}
	if s.live.Load() != 1 {
		t.Errorf("live = %d, want 1 (one aggregate for two getOrCreate calls)", s.live.Load())
	}

	a.bytes.Add(1000)
	a.bytes.Add(500)
	if got := b.bytes.Load(); got != 1500 {
		t.Errorf("bytes via the shared aggregate = %d, want 1500", got)
	}

	if got, ok := s.get("test-1"); !ok || got != a {
		t.Errorf("get returned (%p, %v), want the same aggregate", got, ok)
	}
}

// TestUploadStoreCapRejectsCreate fills the store to the live cap and checks the
// next create is refused (bounding the map under a minted-id flood).
func TestUploadStoreCapRejectsCreate(t *testing.T) {
	s := NewUploadStore()
	for i := 0; i < maxLiveUploads; i++ {
		id := "id-" + strconv.Itoa(i)
		s.markIssued(id)
		if _, ok := s.getOrCreate(id); !ok {
			t.Fatalf("create %d below the cap was refused", i)
		}
	}
	if s.live.Load() != maxLiveUploads {
		t.Fatalf("live = %d, want %d", s.live.Load(), maxLiveUploads)
	}
	s.markIssued("one-too-many")
	if _, ok := s.getOrCreate("one-too-many"); ok {
		t.Errorf("create past the cap succeeded, want refusal")
	}
}

// TestUploadStoreSweepReapsIdle ages an aggregate past the TTL and checks the
// sweeper deletes it and decrements the live count.
func TestUploadStoreSweepReapsIdle(t *testing.T) {
	s := NewUploadStore()
	s.markIssued("idle")
	a, _ := s.getOrCreate("idle")
	// Backdate the last touch well past the TTL (arithmetic is on the monotonic
	// clock, so this holds regardless of how long the process has been up).
	a.lastTouchMono.Store(monoNanos() - int64(2*uploadIDTTL))

	s.markIssued("fresh")
	if _, ok := s.getOrCreate("fresh"); !ok {
		t.Fatal("fresh create failed")
	}

	s.sweep(uploadIDTTL)

	if _, ok := s.get("idle"); ok {
		t.Error("idle aggregate survived the sweep")
	}
	if _, ok := s.get("fresh"); !ok {
		t.Error("fresh aggregate was wrongly reaped")
	}
	if s.live.Load() != 1 {
		t.Errorf("live = %d after sweeping one of two, want 1", s.live.Load())
	}
}

// TestUploadStoreMint checks minted ids are unique, issued (so getOrCreate accepts
// them), and carry the opaque prefix.
func TestUploadStoreMint(t *testing.T) {
	s := NewUploadStore()
	seen := make(map[string]bool)
	for i := 0; i < 100; i++ {
		id := s.Mint()
		if id == "" {
			t.Fatal("Mint returned empty")
		}
		if len(id) < 8 || id[:4] != "gmu_" {
			t.Fatalf("minted id %q lacks the gmu_ prefix", id)
		}
		if seen[id] {
			t.Fatalf("Mint returned a duplicate id %q", id)
		}
		seen[id] = true
		if !s.isIssued(id) {
			t.Fatalf("minted id %q is not marked issued", id)
		}
		if _, ok := s.getOrCreate(id); !ok {
			t.Fatalf("getOrCreate rejected the freshly minted id %q", id)
		}
	}
}

// TestUploadAggActiveTime checks the active measurement clock (the upload rate's
// denominator): recordChunk accrues the wall gap between consecutive chunks but
// EXCLUDES a dead-zone gap longer than activeGapCap (establishment / reconnect /
// stall), so TIME reflects only the time bytes were actually flowing — never a wall
// span. Synthetic timestamps make it deterministic (no sleeps).
func TestUploadAggActiveTime(t *testing.T) {
	var a uploadAgg
	const ms = int64(time.Millisecond)
	gapCap := int64(activeGapCap)

	// First chunk starts the clock; no prior chunk → no active time yet.
	a.recordChunk(1000*ms, 100)
	if got := a.activeNanos.Load(); got != 0 {
		t.Fatalf("active after the first chunk = %d, want 0", got)
	}

	// A short gap (well under the cap) is counted in full.
	a.recordChunk(1000*ms+50*ms, 100)
	if got := a.activeNanos.Load(); got != 50*ms {
		t.Fatalf("active after a 50ms gap = %d, want %d", got, 50*ms)
	}

	// A dead zone (gap > cap) is excluded entirely.
	deadAt := 1000*ms + 50*ms + gapCap + ms
	a.recordChunk(deadAt, 100)
	if got := a.activeNanos.Load(); got != 50*ms {
		t.Fatalf("active after a dead-zone gap = %d, want %d (gap excluded)", got, 50*ms)
	}

	// Transfer resumes; a short gap after the dead zone counts again.
	a.recordChunk(deadAt+10*ms, 100)
	if got := a.activeNanos.Load(); got != 60*ms {
		t.Fatalf("active after resume = %d, want %d", got, 60*ms)
	}
	if got := a.bytes.Load(); got != 400 {
		t.Errorf("bytes = %d, want 400 (every chunk counts regardless of gap)", got)
	}
}

// TestUploadAggActiveTimeConcurrent checks the lock-free active clock is sound
// across a test's parallel lanes — the real shape of a multi-stream upload. Lanes
// stamp with the actual monotonic clock (as the drain path does), so reorder windows
// between a monoNanos read and its Swap are sub-µs; the per-chunk gaps telescope into
// one per-id timeline. The point is race-safety (run with -race) and exact byte
// accounting; active time must land in a sane range (positive, not exceeding the
// test's own wall span by more than slack), never double the work.
func TestUploadAggActiveTimeConcurrent(t *testing.T) {
	var a uploadAgg
	const lanes, perLane = 8, 500

	startWall := monoNanos()
	var wg sync.WaitGroup
	for l := 0; l < lanes; l++ {
		wg.Add(1)
		go func() {
			defer wg.Done()
			for i := 0; i < perLane; i++ {
				a.recordChunk(monoNanos(), 64) // real clock, like discardSink.Write
			}
		}()
	}
	wg.Wait()
	wallSpan := monoNanos() - startWall

	got := a.activeNanos.Load()
	if got < 0 {
		t.Fatalf("concurrent active time = %d, want >= 0", got)
	}
	// Active time is wall time minus dead zones, so it can never exceed the window the
	// chunks actually spanned (plus a small slack for sub-µs Swap reorderings).
	if slack := int64(time.Millisecond); got > wallSpan+slack {
		t.Errorf("concurrent active time = %d exceeds wall span %d (+slack)", got, wallSpan)
	}
	if want := int64(lanes * perLane * 64); a.bytes.Load() != want {
		t.Errorf("bytes = %d, want %d (every chunk counted once, no double count)", a.bytes.Load(), want)
	}
}

// TestUploadStoreDeleteIdempotent checks a double delete never double-decrements live.
func TestUploadStoreDeleteIdempotent(t *testing.T) {
	s := NewUploadStore()
	s.markIssued("d")
	s.getOrCreate("d")
	s.delete("d")
	s.delete("d") // no-op
	s.delete("never-existed")
	if s.live.Load() != 0 {
		t.Errorf("live = %d after idempotent deletes, want 0", s.live.Load())
	}
}
