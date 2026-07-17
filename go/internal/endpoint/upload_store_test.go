package endpoint

import (
	"encoding/base64"
	"sync"
	"testing"
	"time"
)

func TestUploadStoreRejectsForgedID(t *testing.T) {
	s := NewUploadStore()
	if agg, ok := s.getOrCreate("never-minted"); ok || agg != nil {
		t.Fatalf("getOrCreate on a forged id = (%v, %v), want (nil, false)", agg, ok)
	}
	if s.live.Load() != 0 {
		t.Errorf("live = %d after a rejected create, want 0", s.live.Load())
	}
}

func TestUploadStoreMintAllocatesNoState(t *testing.T) {
	s := NewUploadStore()
	for i := 0; i < 10_000; i++ {
		if s.Mint() == "" {
			t.Fatalf("mint %d failed", i)
		}
	}
	if s.live.Load() != 0 {
		t.Fatalf("minting allocated %d live aggregates, want 0", s.live.Load())
	}
}

func TestUploadStoreRejectsTamperedAndExpiredID(t *testing.T) {
	s := NewUploadStore()
	id := s.Mint()
	raw, err := base64.RawURLEncoding.DecodeString(id[4:])
	if err != nil {
		t.Fatal(err)
	}
	raw[len(raw)-1] ^= 1
	tampered := "gmu_" + base64.RawURLEncoding.EncodeToString(raw)
	if _, ok := s.getOrCreate(tampered); ok {
		t.Fatal("tampered id created an aggregate")
	}
	var nonce [16]byte
	expired := s.signID(monoNanos()-int64(uploadTokenTTL)-int64(time.Second), nonce)
	if _, ok := s.getOrCreate(expired); ok {
		t.Fatal("expired id created an aggregate")
	}
}

// TestUploadStoreCreateIsIdempotent checks that POST and WS (or repeated lanes)
// carrying the same minted id all resolve to ONE aggregate, and that counting works.
func TestUploadStoreCreateIsIdempotent(t *testing.T) {
	s := NewUploadStore()
	id := s.Mint()

	a, ok := s.getOrCreate(id)
	if !ok || a == nil {
		t.Fatalf("first getOrCreate failed: ok=%v", ok)
	}
	b, ok := s.getOrCreate(id)
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

	if got, ok := s.get(id); !ok || got != a {
		t.Errorf("get returned (%p, %v), want the same aggregate", got, ok)
	}
}

// TestUploadStoreCapRejectsCreate fills the store to the live cap and checks the
// next create is refused (bounding the map under a minted-id flood).
func TestUploadStoreCapRejectsCreate(t *testing.T) {
	s := NewUploadStore()
	for i := 0; i < maxLiveUploads; i++ {
		id := s.Mint()
		if _, ok := s.getOrCreate(id); !ok {
			t.Fatalf("create %d below the cap was refused", i)
		}
	}
	if s.live.Load() != maxLiveUploads {
		t.Fatalf("live = %d, want %d", s.live.Load(), maxLiveUploads)
	}
	if _, ok := s.getOrCreate(s.Mint()); ok {
		t.Errorf("create past the cap succeeded, want refusal")
	}
}

func TestUploadStorePerOwnerCapAndOwnership(t *testing.T) {
	s := NewUploadStore()
	owner := "192.0.2.1"
	var first string
	for i := 0; i < maxLiveUploadsPerClient; i++ {
		id := s.Mint()
		if i == 0 {
			first = id
		}
		if _, access := s.getOrCreateFor(id, owner); access != uploadAccessOK {
			t.Fatalf("owner create %d = %v", i, access)
		}
	}
	if _, access := s.getOrCreateFor(s.Mint(), owner); access != uploadAccessClientFull {
		t.Fatalf("owner overflow = %v", access)
	}
	if _, access := s.getOrCreateFor(first, "192.0.2.2"); access != uploadAccessOwnerMismatch {
		t.Fatalf("owner mismatch = %v", access)
	}
	if _, access := s.getOrCreateFor(s.Mint(), "192.0.2.2"); access != uploadAccessOK {
		t.Fatalf("independent owner rejected = %v", access)
	}
}

func TestUploadStoreSweepReleasesOwnerCapacity(t *testing.T) {
	s := NewUploadStore()
	owner := "192.0.2.1"
	for i := 0; i < maxLiveUploadsPerClient; i++ {
		agg, access := s.getOrCreateFor(s.Mint(), owner)
		if access != uploadAccessOK {
			t.Fatal(access)
		}
		agg.lastTouchMono.Store(monoNanos() - int64(2*uploadIDTTL))
	}
	s.sweep(uploadIDTTL)
	if _, access := s.getOrCreateFor(s.Mint(), owner); access != uploadAccessOK {
		t.Fatalf("owner capacity not released: %v", access)
	}
}

// TestUploadStoreSweepReapsIdle ages an aggregate past the TTL and checks the
// sweeper deletes it and decrements the live count.
func TestUploadStoreSweepReapsIdle(t *testing.T) {
	s := NewUploadStore()
	idleID := s.Mint()
	a, _ := s.getOrCreate(idleID)
	// Backdate the last touch well past the TTL (arithmetic is on the monotonic
	// clock, so this holds regardless of how long the process has been up).
	a.lastTouchMono.Store(monoNanos() - int64(2*uploadIDTTL))

	freshID := s.Mint()
	if _, ok := s.getOrCreate(freshID); !ok {
		t.Fatal("fresh create failed")
	}

	s.sweep(uploadIDTTL)

	if _, ok := s.get(idleID); ok {
		t.Error("idle aggregate survived the sweep")
	}
	if _, ok := s.get(freshID); !ok {
		t.Error("fresh aggregate was wrongly reaped")
	}
	if s.live.Load() != 1 {
		t.Errorf("live = %d after sweeping one of two, want 1", s.live.Load())
	}
}

func TestUploadStoreSweepPreservesActivePost(t *testing.T) {
	s := NewUploadStore()
	id := s.Mint()
	agg, _ := s.getOrCreate(id)
	agg.changePosts(1)
	agg.lastTouchMono.Store(monoNanos() - int64(2*uploadIDTTL))
	s.sweep(uploadIDTTL)
	if _, ok := s.get(id); !ok {
		t.Fatal("active upload was reaped")
	}
	agg.changePosts(-1)
	s.sweep(uploadIDTTL)
	if _, ok := s.get(id); ok {
		t.Fatal("idle upload survived after its final post exited")
	}
}

// TestUploadStoreMint checks minted ids are unique, authenticated, and opaque.
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
		if _, ok := s.getOrCreate(id); !ok {
			t.Fatalf("getOrCreate rejected the freshly minted id %q", id)
		}
	}
}

// TestUploadAggElapsedTime checks that TIME is wall time since the first byte,
// including a long transfer stall. Synthetic timestamps keep it deterministic.
func TestUploadAggElapsedTimeIncludesStalls(t *testing.T) {
	var a uploadAgg
	const ms = int64(time.Millisecond)

	// First chunk starts the clock.
	a.recordChunk(1000*ms, 100)
	if got := a.elapsedNanos(1000 * ms); got != 0 {
		t.Fatalf("elapsed at the first chunk = %d, want 0", got)
	}

	if got := a.elapsedNanos(1050 * ms); got != 50*ms {
		t.Fatalf("elapsed after 50ms = %d, want %d", got, 50*ms)
	}

	// No recordChunk occurs during this 2s stall, but it remains in TIME.
	if got := a.elapsedNanos(3050 * ms); got != 2050*ms {
		t.Fatalf("elapsed after stall = %d, want %d", got, 2050*ms)
	}
	a.recordChunk(3060*ms, 100)
	if got := a.elapsedNanos(3060 * ms); got != 2060*ms {
		t.Fatalf("elapsed after resume = %d, want %d", got, 2060*ms)
	}
	if got := a.bytes.Load(); got != 200 {
		t.Errorf("bytes = %d, want 200", got)
	}
}

// TestUploadAggElapsedTimeConcurrent checks race-safe first-byte anchoring and
// exact accounting across the parallel lanes used by a real upload.
func TestUploadAggElapsedTimeConcurrent(t *testing.T) {
	var a uploadAgg
	const lanes, perLane = 8, 500

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
	if got := a.elapsedNanos(monoNanos()); got < 0 {
		t.Fatalf("concurrent elapsed time = %d, want >= 0", got)
	}
	if want := int64(lanes * perLane * 64); a.bytes.Load() != want {
		t.Errorf("bytes = %d, want %d (every chunk counted once, no double count)", a.bytes.Load(), want)
	}
}

// TestUploadStoreDeleteIdempotent checks a double delete never double-decrements live.
func TestUploadStoreDeleteIdempotent(t *testing.T) {
	s := NewUploadStore()
	id := s.Mint()
	s.getOrCreate(id)
	s.delete(id)
	s.delete(id) // no-op
	s.delete("never-existed")
	if s.live.Load() != 0 {
		t.Errorf("live = %d after idempotent deletes, want 0", s.live.Load())
	}
}

// TestUploadStoreSweepBoundary checks the sweeper's idle-cutoff comparison
// near the TTL edge: an aggregate touched just inside the TTL survives, one
// touched just past it is reaped. (An exact-nanosecond tie isn't asserted —
// sweep's own monoNanos() call advances between statements, so a true tie is
// inherently racy; a safe margin on both sides still pins down the same
// "< cutoff" comparison sweep uses.)
func TestUploadStoreSweepBoundary(t *testing.T) {
	s := NewUploadStore()
	const ttl = 200 * time.Millisecond
	const margin = 50 * time.Millisecond
	now := monoNanos()

	survivorID := s.Mint()
	survivor, _ := s.getOrCreate(survivorID)
	survivor.lastTouchMono.Store(now - int64(ttl) + int64(margin)) // idle age < ttl

	expiredID := s.Mint()
	expired, _ := s.getOrCreate(expiredID)
	expired.lastTouchMono.Store(now - int64(ttl) - int64(margin)) // idle age > ttl

	s.sweep(ttl)

	if _, ok := s.get(survivorID); !ok {
		t.Error("aggregate just inside the TTL was reaped, want survival")
	}
	if _, ok := s.get(expiredID); ok {
		t.Error("aggregate just past the TTL survived, want reaping")
	}
}

// TestUploadStoreCapAllowsCreateAfterDeleteFreesSpace checks the live cap is
// rechecked on every first-touch create rather than latched permanently:
// freeing a slot (delete or sweep) lets a new id through again.
func TestUploadStoreCapAllowsCreateAfterDeleteFreesSpace(t *testing.T) {
	s := NewUploadStore()
	ids := make([]string, maxLiveUploads)
	for i := 0; i < maxLiveUploads; i++ {
		id := s.Mint()
		ids[i] = id
		if _, ok := s.getOrCreate(id); !ok {
			t.Fatalf("create %d below the cap was refused", i)
		}
	}
	blocked := s.Mint()
	if _, ok := s.getOrCreate(blocked); ok {
		t.Fatal("create at the cap unexpectedly succeeded")
	}

	s.delete(ids[0])
	if _, ok := s.getOrCreate(blocked); !ok {
		t.Error("create after a delete freed a slot was still refused")
	}
	if s.live.Load() != maxLiveUploads {
		t.Errorf("live = %d, want %d after freeing and refilling one slot", s.live.Load(), maxLiveUploads)
	}
}

// TestUploadStoreConcurrentGetAndSweep races get/getOrCreate against a
// sweeper reaping a mix of already-expired and still-fresh aggregates — the
// real shape of production traffic hitting the store while RunSweeper ticks.
// The point is race-safety (run with -race) and that `live` never desyncs
// from the actual shard contents.
func TestUploadStoreConcurrentGetAndSweep(t *testing.T) {
	s := NewUploadStore()
	const n = 200
	ids := make([]string, n)
	for i := range ids {
		id := s.Mint()
		ids[i] = id
		agg, _ := s.getOrCreate(id)
		if i%2 == 0 {
			// Half start already past the TTL so sweep can reap them
			// immediately while readers are still hammering the store.
			agg.lastTouchMono.Store(monoNanos() - int64(time.Second))
		}
	}

	stop := make(chan struct{})
	sweeperDone := make(chan struct{})
	go func() {
		defer close(sweeperDone)
		for {
			select {
			case <-stop:
				return
			default:
				s.sweep(500 * time.Millisecond)
			}
		}
	}()

	var wg sync.WaitGroup
	for w := 0; w < 4; w++ {
		wg.Add(1)
		go func() {
			defer wg.Done()
			for i := 0; i < 500; i++ {
				id := ids[i%n]
				if agg, ok := s.get(id); ok {
					agg.lastTouchMono.Store(monoNanos())
				}
				s.getOrCreate(id)
			}
		}()
	}
	wg.Wait()
	close(stop)
	<-sweeperDone

	if got := s.live.Load(); got < 0 {
		t.Fatalf("live went negative: %d", got)
	}
	var counted int32
	for i := range s.shards {
		sh := &s.shards[i]
		sh.mu.Lock()
		counted += int32(len(sh.m))
		sh.mu.Unlock()
	}
	if counted != s.live.Load() {
		t.Errorf("live = %d, but shards contain %d aggregates", s.live.Load(), counted)
	}
}

// TestUploadAggFirstChunkAnchorNeverMoves checks that later lanes cannot move
// the elapsed clock's first-byte anchor, while every chunk still counts.
func TestUploadAggFirstChunkAnchorNeverMoves(t *testing.T) {
	var a uploadAgg
	a.recordChunk(1000, 100)
	a.recordChunk(500, 100)
	a.recordChunk(1100, 100)
	if got := a.firstChunkMono.Load(); got != 1000 {
		t.Fatalf("firstChunkMono = %d, want 1000", got)
	}
	if got := a.elapsedNanos(1200); got != 200 {
		t.Fatalf("elapsed = %d, want 200", got)
	}
	if got := a.bytes.Load(); got != 300 {
		t.Errorf("bytes = %d, want 300", got)
	}
}
