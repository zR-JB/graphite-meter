package endpoint

import (
	"context"
	"crypto/rand"
	"encoding/base64"
	"hash/fnv"
	"sync"
	"sync/atomic"
	"time"
)

// UploadStore holds per-test state shared between a POST /upload lane and its
// /ws/upload progress socket — separate TCP connections joined by ?id= — so the
// SERVER's drained byte count, not the browser's onprogress, is authoritative.
//
// The map is sharded so a POST's hot-path byte add never contends a lock: the
// add is a lockless atomic on *uploadAgg; only first-touch create and sweep
// delete take a shard mutex.
//
// An aggregate is created on first touch (POST or WS, whichever arrives first)
// and reaped only by the TTL sweeper — a refcount fast-path delete once raced a
// straggler lane into a fresh zero-byte aggregate that lost its bytes.
//
// Creation is gated to ids minted at /upload/session (markIssued): a flood of
// forged ids on this auth-less bus creates no state, closing both the cross-tab
// progress-read leak and the maxLiveUploads-amplified DoS vector.
type UploadStore struct {
	shards [uploadShardCount]uploadShard
	issued sync.Map     // id (string) -> issue time (mono ns); only minted ids may create an agg
	live   atomic.Int32 // count of live aggregates, bounding the map (maxLiveUploads)
}

type uploadShard struct {
	mu sync.Mutex
	m  map[string]*uploadAgg
}

// uploadAgg is one test's cross-connection accumulator; every field is atomic so
// the POST drain path (recordChunk) and the WS ticker (bytes.Load every 100 ms)
// never share a lock.
//
// activeNanos is the server's own rate denominator: wall time bytes were
// actually flowing, with dead zones (handshake, reconnect, stall) excluded —
// NOT the wall span between first and last frame, so a stall can never inflate
// the rate by stretching a wall-clock denominator instead.
type uploadAgg struct {
	bytes         atomic.Int64 // cumulative drained bytes across ALL this id's POST lanes
	activeNanos   atomic.Int64 // cumulative ACTIVE measurement time (ns): wall gaps between chunks, dead zones excluded
	lastChunkMono atomic.Int64 // mono ns of the previous drained chunk (drain-path ONLY) — the active-clock anchor
	lastTouchMono atomic.Int64 // mono ns of the last chunk OR last WS tick — the sweeper's idle clock
	posts         atomic.Int32 // live POST lanes for this id (diagnostics; NOT a deleter)
	done          atomic.Bool  // UPLOAD_COMPLETE already sent — idempotency guard for the WS finalizer
}

// recordChunk folds one drained chunk (n bytes at monotonic time now) into the
// aggregate: bytes always add; the wall gap since the previous chunk adds to
// activeNanos only when at most activeGapCap (a longer gap is a dead zone —
// establishment, reconnect, stall — and is excluded).
//
// The forward-only CAS on lastChunkMono makes this safe across a test's
// parallel POST lanes without a lock: the clock never rewinds, so gaps from
// interleaved lanes telescope into one monotonic per-id timeline, never
// exceeding the true wall span. A stale/reordered stamp (now <= the latest)
// contributes no time rather than rewinding the clock — any residual skew can
// only under-report the rate, never inflate it.
func (a *uploadAgg) recordChunk(now int64, n int) {
	for {
		prev := a.lastChunkMono.Load()
		if now <= prev {
			break // stale / reordered stamp — don't rewind the clock or double-count
		}
		if a.lastChunkMono.CompareAndSwap(prev, now) {
			if prev != 0 {
				if gap := now - prev; gap <= int64(activeGapCap) {
					a.activeNanos.Add(gap)
				}
			}
			break
		}
		// CAS lost to a concurrent lane; retry against the fresh clock value.
	}
	a.bytes.Add(int64(n))
	a.lastTouchMono.Store(now) // keeps the id from looking idle to the sweeper
}

const (
	// uploadShardCount shards the map to spread first-touch/sweep lock contention
	// across concurrent tests. Power of two; fnv32 % count.
	uploadShardCount = 32
	// maxLiveUploads bounds the aggregate map so a burst of minted ids can't grow
	// memory without bound.
	maxLiveUploads = 1000
	// uploadIDTTL: an aggregate idle this long (no chunk, no WS tick) is reaped by
	// the sweeper — covers abort, tab close, crash, or an orphaned WS with no POST.
	uploadIDTTL = 30 * time.Second
	// issuedIDTTL forgets a minted-but-unconsumed id after this long, so `issued`
	// can't grow forever. Pruning it never breaks a running test — getOrCreate
	// returns an existing aggregate without re-checking issued.
	issuedIDTTL = 2 * time.Minute
	// uploadSweepInterval is how often RunSweeper scans for idle aggregates.
	uploadSweepInterval = 5 * time.Second
	// activeGapCap is the longest inter-chunk gap counted as continuous transfer
	// (recordChunk); longer gaps are dead zones (handshake, reconnect, stall) and
	// are excluded. A saturated link keeps real gaps far below this; a lane
	// reconnect (client backoff + handshake) sits well above it. A request-
	// buffering proxy can defeat this by releasing data in bursts — an untrusted
	// topology, detectable by comparing activeNanos to the wall span.
	activeGapCap = 250 * time.Millisecond
)

// NewUploadStore builds an empty store with its shard maps initialised.
func NewUploadStore() *UploadStore {
	s := &UploadStore{}
	for i := range s.shards {
		s.shards[i].m = make(map[string]*uploadAgg)
	}
	return s
}

// monoNanos is the store's monotonic clock: ns since process start (startMono is
// declared in ping.go, same package). time.Since reads the monotonic clock, so the
// value never jumps with wall-clock changes — safe for idle-age comparisons.
func monoNanos() int64 { return int64(time.Since(startMono)) }

// shard returns the shard owning id (hash computed OUTSIDE any lock).
func (s *UploadStore) shard(id string) *uploadShard {
	h := fnv.New32a()
	_, _ = h.Write([]byte(id))
	return &s.shards[h.Sum32()%uploadShardCount]
}

// Mint generates a fresh opaque upload-session token (128 bits of crypto entropy,
// URL-safe so it rides ?id= without escaping), records it as issued, and returns
// it. /upload/session calls this once per upload stage; only minted tokens can create an
// aggregate, which is the store's primary abuse defence.
func (s *UploadStore) Mint() string {
	var b [16]byte
	if _, err := rand.Read(b[:]); err != nil {
		// crypto/rand failing is effectively fatal; return "" so the client simply
		// runs without server-authoritative upload rather than getting a weak token.
		return ""
	}
	id := "gmu_" + base64.RawURLEncoding.EncodeToString(b[:])
	s.markIssued(id)
	return id
}

// markIssued records that the server minted id at /upload/session, so a later POST/WS
// carrying it may create an aggregate. Idempotent.
func (s *UploadStore) markIssued(id string) {
	if id == "" {
		return
	}
	s.issued.Store(id, monoNanos())
}

// isIssued reports whether id was minted by this server and not yet pruned.
func (s *UploadStore) isIssued(id string) bool {
	if id == "" {
		return false
	}
	_, ok := s.issued.Load(id)
	return ok
}

// getOrCreate returns the aggregate for id, creating it on first touch. It returns
// (nil, false) when id is empty, was never minted (and no aggregate exists yet), or
// the live-aggregate cap is reached. An EXISTING aggregate is always returned —
// the issued gate and the cap apply only to the first create, so pruning `issued`
// or hitting the cap mid-test never strands a running test's later lanes. Every
// successful call bumps lastTouchMono so an active id never looks idle to the sweeper.
func (s *UploadStore) getOrCreate(id string) (*uploadAgg, bool) {
	if id == "" {
		return nil, false
	}
	sh := s.shard(id)
	sh.mu.Lock()
	if agg, ok := sh.m[id]; ok {
		sh.mu.Unlock()
		agg.lastTouchMono.Store(monoNanos())
		return agg, true
	}
	// First touch — gate creation on a minted id and the live cap.
	if !s.isIssued(id) || s.live.Load() >= maxLiveUploads {
		sh.mu.Unlock()
		return nil, false
	}
	agg := &uploadAgg{}
	agg.lastTouchMono.Store(monoNanos())
	sh.m[id] = agg
	s.live.Add(1)
	sh.mu.Unlock()
	return agg, true
}

// get returns the existing aggregate for id without creating one.
func (s *UploadStore) get(id string) (*uploadAgg, bool) {
	if id == "" {
		return nil, false
	}
	sh := s.shard(id)
	sh.mu.Lock()
	agg, ok := sh.m[id]
	sh.mu.Unlock()
	return agg, ok
}

// delete removes id's aggregate if present. Idempotent — a second delete (or a
// delete racing the sweeper) is a no-op and never double-decrements `live`.
func (s *UploadStore) delete(id string) {
	sh := s.shard(id)
	sh.mu.Lock()
	if _, ok := sh.m[id]; ok {
		delete(sh.m, id)
		s.live.Add(-1)
	}
	sh.mu.Unlock()
}

// sweep reaps every aggregate idle longer than ttl (no chunk, no WS tick) and
// forgets every minted id older than issuedIDTTL. It is the store's only deleter
// of aggregate state; bytes are never lost because the sole authoritative read is
// the cumulative value at the client's BYE, long before idle reaping.
func (s *UploadStore) sweep(ttl time.Duration) {
	now := monoNanos()
	aggCutoff := now - int64(ttl)
	for i := range s.shards {
		sh := &s.shards[i]
		sh.mu.Lock()
		for id, agg := range sh.m {
			if agg.lastTouchMono.Load() < aggCutoff {
				delete(sh.m, id)
				s.live.Add(-1)
			}
		}
		sh.mu.Unlock()
	}
	issuedCutoff := now - int64(issuedIDTTL)
	s.issued.Range(func(k, v any) bool {
		if at, ok := v.(int64); ok && at < issuedCutoff {
			s.issued.Delete(k)
		}
		return true
	})
}

// RunSweeper reaps idle aggregates until ctx is cancelled. Start it once per store
// in its own goroutine (like Meter.Run). It reaps map STATE only — goroutine
// lifetimes on the /ws/upload bus are bounded separately by that handler's idle
// read deadline (10s timeout on /ws/upload bus handler).
func (s *UploadStore) RunSweeper(ctx context.Context) {
	ticker := time.NewTicker(uploadSweepInterval)
	defer ticker.Stop()
	for {
		select {
		case <-ctx.Done():
			return
		case <-ticker.C:
			s.sweep(uploadIDTTL)
		}
	}
}
