package endpoint

import (
	"context"
	"hash/fnv"
	"sync"
	"sync/atomic"
	"time"
)

// uploadStore holds the per-test shared state that makes the SERVER-side drained
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
type uploadStore struct {
	shards [uploadShardCount]uploadShard
	issued sync.Map     // id (string) -> issue time (mono ns); only minted ids may create an agg
	live   atomic.Int32 // count of live aggregates, bounding the map (maxLiveUploads)
}

type uploadShard struct {
	mu sync.Mutex
	m  map[string]*uploadAgg
}

// uploadAgg is one test's cross-connection accumulator. Every field is an atomic
// so the POST drain hot path (bytes.Add per chunk) and the WS ticker (bytes.Load
// every 100 ms) never share a lock.
type uploadAgg struct {
	bytes         atomic.Int64 // cumulative drained bytes across ALL this id's POST lanes
	firstByteMono atomic.Int64 // mono ns of the first chunk, CAS-set once (0 = none yet)
	lastTouchMono atomic.Int64 // mono ns of the last chunk OR last WS tick — the sweeper's idle clock
	posts         atomic.Int32 // live POST lanes for this id (diagnostics; NOT a deleter)
	done          atomic.Bool  // UPLOAD_COMPLETE already sent — idempotency guard for the WS finalizer
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
)

// NewUploadStore builds an empty store with its shard maps initialised.
func NewUploadStore() *uploadStore {
	s := &uploadStore{}
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
func (s *uploadStore) shard(id string) *uploadShard {
	h := fnv.New32a()
	_, _ = h.Write([]byte(id))
	return &s.shards[h.Sum32()%uploadShardCount]
}

// markIssued records that the server minted id at /preflight, so a later POST/WS
// carrying it may create an aggregate. Idempotent.
func (s *uploadStore) markIssued(id string) {
	if id == "" {
		return
	}
	s.issued.Store(id, monoNanos())
}

// isIssued reports whether id was minted by this server and not yet pruned.
func (s *uploadStore) isIssued(id string) bool {
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
func (s *uploadStore) getOrCreate(id string) (*uploadAgg, bool) {
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
func (s *uploadStore) get(id string) (*uploadAgg, bool) {
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
func (s *uploadStore) delete(id string) {
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
func (s *uploadStore) sweep(ttl time.Duration) {
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
func (s *uploadStore) RunSweeper(ctx context.Context) {
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
