package endpoint

import (
	"strconv"
	"testing"
)

// TestUploadStoreRejectsUnissuedID guards the abuse defence: an id the server never
// minted at /preflight cannot create an aggregate (§3, §9).
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
