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

// UploadStore holds the per-test shared state that makes the SERVER-side drained
// byte count the authoritative upload result (docs/UPLOAD_ARCHITECTURE.md §3).
// The upload's HTTP POST lanes and its /ws/upload progress socket are SEPARATE
// TCP connections; both carry the same ?id= and meet here. Each POST drains into
// the same per-id uploadAgg (counting), and the progress bus reads that aggregate
// to push BYTES_RECEIVED/UPLOAD_COMPLETE.
//
// The map is sharded so the hot path (a POST's per-chunk byte add) never contends
// on a single lock: the add itself is a lockless atomic on the *uploadAgg; only
// the rare first-touch create / sweep delete take a shard mutex.
//
// Lifecycle is deliberately simple and race-free (§3, §9): an aggregate is created
// on FIRST TOUCH by whichever of the POST or the WS arrives first (order-free), and
// the TTL sweeper is the ONLY deleter — there is no refcount fast-path delete (that
// raced a slow straggler lane into a fresh zero-byte aggregate whose bytes were
// never reported). State lives from first-touch until uploadIDTTL idle, monotonic,
// never resurrected mid-test.
//
// Aggregate creation is gated to ids the server MINTED at /preflight (markIssued):
// a flood of forged ids on this auth-less bus then creates no state, which both
// stops cross-tab reads of a victim's progress stream and defuses the
// maxLiveUploads-amplified DoS/lockout vector (§9).
type UploadStore struct {
	shards [uploadShardCount]uploadShard
	issued sync.Map     // id (string) -> issue time (mono ns); only minted ids may create an agg
	live   atomic.Int32 // count of live aggregates, bounding the map (maxLiveUploads)
}

type uploadShard struct {
	mu sync.Mutex
	m  map[string]*uploadAgg
}

// uploadAgg is one test's cross-connection accumulator. Every field is an atomic
// so the POST drain hot path (recordChunk per chunk) and the WS ticker (bytes.Load
// every 100 ms) never share a lock.
//
// activeNanos is the heart of the measurement: it is the SERVER's own measurement
// clock — wall time during which bytes were ACTUALLY being drained, with dead zones
// (TCP/handshake establishment, a lane reconnect, an idle stall) excluded. The
// /ws/upload bus ships it in the ;TIME field beside `bytes`, both sampled at the one
// drain point, so the client derives the upload rate as Δbytes / ΔactiveNanos — the
// upload mirror of the download worker, where the client already measures bytes and
// time together at the single read point (docs/UPLOAD_ARCHITECTURE.md §5). It is NOT
// the wall span between the first and last frame, so a stall/reconnect/early-finish
// can never stretch the denominator with idle time.
type uploadAgg struct {
	bytes         atomic.Int64 // cumulative drained bytes across ALL this id's POST lanes
	activeNanos   atomic.Int64 // cumulative ACTIVE measurement time (ns): wall gaps between chunks, dead zones excluded
	lastChunkMono atomic.Int64 // mono ns of the previous drained chunk (drain-path ONLY) — the active-clock anchor
	lastTouchMono atomic.Int64 // mono ns of the last chunk OR last WS tick — the sweeper's idle clock
	posts         atomic.Int32 // live POST lanes for this id (diagnostics; NOT a deleter)
	done          atomic.Bool  // UPLOAD_COMPLETE already sent — idempotency guard for the WS finalizer
}

// recordChunk folds one drained chunk (n bytes, drained at monotonic time `now` ns)
// into the aggregate. It always adds the bytes; it advances the active measurement
// clock by the wall gap since the PREVIOUS chunk — but only when that gap is at most
// activeGapCap. A larger gap is a dead zone (establishment, a lane reconnect after
// the client's restart backoff, an idle stall) and is excluded, so activeNanos only
// ever counts time bytes were really flowing.
//
// Concurrency-safe across a test's parallel POST lanes without a lock, via a
// forward-only CAS on lastChunkMono: the clock never moves backward, so the per-chunk
// gaps telescope across interleaved lanes into ONE monotonic per-id timeline
// (= last chunk − first chunk − Σ dead-zone gaps), exactly the wall time at least one
// lane was delivering data — and NEVER more (activeNanos ≤ wall span, always). A chunk
// whose stamp lost the race and is now stale (now ≤ the latest) contributes no time
// rather than rewinding the clock and inflating the next gap; the lane that advanced
// the clock already covered that instant. This keeps the measurement conservative:
// any residual skew can only lengthen the denominator (under-report), never inflate
// the rate.
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
	// uploadShardCount shards the aggregate map to spread first-touch/sweep lock
	// contention across many concurrent tests. Power of two; fnv32 % count.
	uploadShardCount = 32
	// maxLiveUploads bounds the aggregate map. Creation past this is refused, so a
	// burst of (minted) ids can't grow memory without bound. Tests stay well under.
	maxLiveUploads = 1000
	// uploadIDTTL: an aggregate with no chunk and no WS tick for this long is reaped
	// by the sweeper — the sole deleter. Covers abort, tab close, worker crash, a
	// delayed RST, and an orphaned WS that never saw a POST.
	uploadIDTTL = 30 * time.Second
	// issuedIDTTL: a minted-but-unconsumed id is forgotten after this, so /preflight
	// minting can't grow `issued` forever. Far longer than a test's upload stage, and
	// pruning it never breaks a running test (getOrCreate returns an EXISTING agg
	// without re-checking issued; the gate only blocks the very first create).
	issuedIDTTL = 2 * time.Minute
	// uploadSweepInterval is how often RunSweeper scans for idle aggregates.
	uploadSweepInterval = 5 * time.Second
	// activeGapCap bounds what counts as continuous transfer for the active
	// measurement clock (recordChunk). The wall gap between two consecutive drained
	// chunks (across all of a test's lanes) is folded into activeNanos only when it is
	// at most this long; a longer gap is a dead zone and is excluded. On a saturated
	// direct-origin link TCP delivers data continuously, so real inter-chunk gaps stay
	// far below this even on slow links (Read returns per arriving segment, not per
	// full buffer); a lane reconnect (≥ the client's ~300 ms restart backoff + a fresh
	// handshake) sits well above it — clean separation that neither clips a healthy
	// transfer nor counts a stall. (A request-buffering proxy can defeat this by
	// releasing the body in bursts; that topology is already declared UNTRUSTED —
	// docs/UPLOAD_ARCHITECTURE.md §8 — and is detectable by comparing activeNanos
	// against the wall span, since a spool collapses the two.)
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
// it. /preflight calls this once per request; only minted tokens can create an
// aggregate, which is the store's primary abuse defence (§9).
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

// markIssued records that the server minted id at /preflight, so a later POST/WS
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
// read deadline (docs/UPLOAD_ARCHITECTURE.md §4).
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
