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

// UploadStore holds per-test state shared between POST /upload lanes and the
// /upload/progress stream — separate requests joined by ?id= — so the
// SERVER's drained byte count, not the browser's onprogress, is authoritative.
//
// The map is sharded so a POST's hot-path byte add never contends a lock: the
// add is a lockless atomic on *uploadAgg; only first-touch create and sweep
// delete take a shard mutex.
//
// An aggregate is created on first touch and deleted after explicit finalization
// or by the TTL sweeper after an abandoned run.
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
// the POST drain path (recordChunk) and the NDJSON stream (bytes.Load every 100 ms)
// never share a lock.
//
// firstChunkMono anchors the server's elapsed-time rate denominator. Clients
// baseline TIME when measurement starts, which excludes warmup while retaining
// every measured pause: congestion, lane turnaround, reconnects and stalls must
// reduce throughput rather than disappear from its denominator.
type uploadAgg struct {
	bytes          atomic.Int64 // cumulative drained bytes across ALL this id's POST lanes
	firstChunkMono atomic.Int64 // mono ns of the first drained chunk; set exactly once
	lastTouchMono  atomic.Int64 // mono ns of the last chunk or progress tick — sweeper idle clock
	posts          atomic.Int32 // live POST lanes for this id (diagnostics; NOT a deleter)
	postsChanged   chan struct{}
	finished       chan struct{} // explicitly closed by DELETE /upload/progress
	finishOnce     sync.Once
}

// recordChunk counts one drained chunk and starts the elapsed clock on the first
// byte. CompareAndSwap makes the anchor safe across parallel POST lanes.
func (a *uploadAgg) recordChunk(now int64, n int) {
	a.firstChunkMono.CompareAndSwap(0, now)
	a.bytes.Add(int64(n))
	a.lastTouchMono.Store(now) // keeps the id from looking idle to the sweeper
}

func (a *uploadAgg) changePosts(delta int32) {
	a.posts.Add(delta)
	select {
	case a.postsChanged <- struct{}{}:
	default:
	}
}

// elapsedNanos returns wall time since the first drained chunk. Sampling this
// clock even while no bytes arrive is intentional: measured stalls count.
func (a *uploadAgg) elapsedNanos(now int64) int64 {
	start := a.firstChunkMono.Load()
	if start == 0 || now <= start {
		return 0
	}
	return now - start
}

const (
	// uploadShardCount shards the map to spread first-touch/sweep lock contention
	// across concurrent tests. Power of two; fnv32 % count.
	uploadShardCount = 32
	// maxLiveUploads bounds the aggregate map so a burst of minted ids can't grow
	// memory without bound.
	maxLiveUploads = 1000
	// uploadIDTTL: an aggregate idle this long (no chunk or progress tick) is
	// reaped by the sweeper after abort, tab close, or crash.
	uploadIDTTL = 30 * time.Second
	// issuedIDTTL forgets a minted-but-unconsumed id after this long, so `issued`
	// can't grow forever. Pruning it never breaks a running test — getOrCreate
	// returns an existing aggregate without re-checking issued.
	issuedIDTTL = 2 * time.Minute
	// uploadSweepInterval is how often RunSweeper scans for idle aggregates.
	uploadSweepInterval = 5 * time.Second
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

// markIssued records that the server minted id at /upload/session, so a later POST/progress
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
	agg := &uploadAgg{finished: make(chan struct{}), postsChanged: make(chan struct{}, 1)}
	agg.lastTouchMono.Store(monoNanos())
	sh.m[id] = agg
	s.live.Add(1)
	sh.mu.Unlock()
	return agg, true
}

// finish marks the client-owned upload stage lifecycle as closed. Completion is
// explicit: the progress stream never guesses from a quiet gap between POSTs.
func (s *UploadStore) finish(id string) bool {
	agg, ok := s.get(id)
	if !ok {
		return false
	}
	agg.finishOnce.Do(func() { close(agg.finished) })
	return true
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

// sweep reaps every aggregate idle longer than ttl and
// forgets every minted id older than issuedIDTTL. It is the store's only deleter
// of aggregate state; bytes are never lost because the sole authoritative read is
// the cumulative value at explicit finalization, long before idle reaping.
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
// in its own goroutine (like Meter.Run). Request contexts independently bound
// progress-handler goroutine lifetimes.
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
