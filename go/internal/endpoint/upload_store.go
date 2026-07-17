package endpoint

import (
	"context"
	"crypto/hmac"
	"crypto/rand"
	"crypto/sha256"
	"encoding/base64"
	"encoding/binary"
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
// An aggregate is created on first touch and retained through explicit
// finalization so a reconnect can replay completion; the TTL sweeper deletes it.
//
// Creation requires a short-lived authenticated id minted at /upload/session.
// Minting is stateless; only live aggregates consume bounded server memory.
type UploadStore struct {
	shards       [uploadShardCount]uploadShard
	tokenKey     [sha256.Size]byte
	tokenKeyOnce sync.Once
	tokenKeyOK   bool
	live         atomic.Int32 // live aggregates retained through completion replay
	ownersMu     sync.Mutex
	byOwner      map[string]int
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
	expired        chan struct{} // closed when idle state is reaped
	finishOnce     sync.Once
	progressActive atomic.Bool
	owner          string
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
	// maxLiveUploadsPerClient leaves ample room for rapid abort/retry cycles while
	// preventing one source from occupying the global aggregate map.
	maxLiveUploadsPerClient = 32
	// uploadIDTTL: an aggregate idle this long (no chunk or progress tick) is
	// reaped by the sweeper after abort, tab close, or crash.
	uploadIDTTL = 30 * time.Second
	// uploadTokenTTL limits how long a minted id may create its aggregate.
	uploadTokenTTL = 2 * time.Minute
	// uploadSweepInterval is how often RunSweeper scans for idle aggregates.
	uploadSweepInterval = 5 * time.Second
)

// NewUploadStore builds an empty store with its shard maps initialised.
func NewUploadStore() *UploadStore {
	s := &UploadStore{byOwner: make(map[string]int)}
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

// Mint generates a URL-safe, authenticated upload-session token without storing
// per-token state. /upload/session calls this once per upload stage.
func (s *UploadStore) Mint() string {
	if !s.ensureTokenKey() {
		return ""
	}
	var nonce [16]byte
	if _, err := rand.Read(nonce[:]); err != nil {
		return ""
	}
	return s.signID(monoNanos(), nonce)
}

func (s *UploadStore) ensureTokenKey() bool {
	s.tokenKeyOnce.Do(func() {
		_, err := rand.Read(s.tokenKey[:])
		s.tokenKeyOK = err == nil
	})
	return s.tokenKeyOK
}

func (s *UploadStore) signID(issued int64, nonce [16]byte) string {
	var payload [8 + len(nonce)]byte
	binary.BigEndian.PutUint64(payload[:8], uint64(issued))
	copy(payload[8:], nonce[:])
	mac := hmac.New(sha256.New, s.tokenKey[:])
	_, _ = mac.Write(payload[:])
	return "gmu_" + base64.RawURLEncoding.EncodeToString(append(payload[:], mac.Sum(nil)...))
}

func (s *UploadStore) validID(id string) bool {
	if len(id) < 4 || id[:4] != "gmu_" || !s.ensureTokenKey() {
		return false
	}
	raw, err := base64.RawURLEncoding.DecodeString(id[4:])
	if err != nil || len(raw) != 8+16+sha256.Size {
		return false
	}
	payload, tag := raw[:24], raw[24:]
	mac := hmac.New(sha256.New, s.tokenKey[:])
	_, _ = mac.Write(payload)
	if !hmac.Equal(tag, mac.Sum(nil)) {
		return false
	}
	issued := int64(binary.BigEndian.Uint64(payload[:8]))
	now := monoNanos()
	return issued > 0 && issued <= now && now-issued <= int64(uploadTokenTTL)
}

// getOrCreate returns the aggregate for id, creating it on first touch. It returns
// (nil, false) when id is invalid (and no aggregate exists yet), or
// the live-aggregate cap is reached. An EXISTING aggregate is always returned —
// authentication and the cap apply only to the first create, so token expiry or
// hitting the cap mid-test never strands a running test's later lanes. Every
// successful call bumps lastTouchMono so upload activity never looks idle to the sweeper.
func (s *UploadStore) getOrCreate(id string) (*uploadAgg, bool) {
	agg, access := s.getOrCreateFor(id, "")
	return agg, access == uploadAccessOK
}

type uploadAccess uint8

const (
	uploadAccessOK uploadAccess = iota
	uploadAccessInvalid
	uploadAccessGlobalFull
	uploadAccessClientFull
	uploadAccessOwnerMismatch
)

func (s *UploadStore) getOrCreateFor(id, owner string) (*uploadAgg, uploadAccess) {
	return s.getOrCreateForActivity(id, owner, true)
}

func (s *UploadStore) getOrCreateForActivity(id, owner string, touch bool) (*uploadAgg, uploadAccess) {
	if id == "" {
		return nil, uploadAccessInvalid
	}
	sh := s.shard(id)
	sh.mu.Lock()
	if agg, ok := sh.m[id]; ok {
		if agg.owner != "" && owner != agg.owner {
			sh.mu.Unlock()
			return nil, uploadAccessOwnerMismatch
		}
		sh.mu.Unlock()
		if touch {
			agg.lastTouchMono.Store(monoNanos())
		}
		return agg, uploadAccessOK
	}
	if !s.validID(id) {
		sh.mu.Unlock()
		return nil, uploadAccessInvalid
	}
	for {
		n := s.live.Load()
		if n >= maxLiveUploads {
			sh.mu.Unlock()
			return nil, uploadAccessGlobalFull
		}
		if s.live.CompareAndSwap(n, n+1) {
			break
		}
	}
	if owner != "" {
		s.ownersMu.Lock()
		if s.byOwner[owner] >= maxLiveUploadsPerClient {
			s.ownersMu.Unlock()
			s.live.Add(-1)
			sh.mu.Unlock()
			return nil, uploadAccessClientFull
		}
		s.byOwner[owner]++
		s.ownersMu.Unlock()
	}
	agg := &uploadAgg{finished: make(chan struct{}), expired: make(chan struct{}), postsChanged: make(chan struct{}, 1), owner: owner}
	agg.lastTouchMono.Store(monoNanos())
	sh.m[id] = agg
	sh.mu.Unlock()
	return agg, uploadAccessOK
}

func (s *UploadStore) releaseOwner(agg *uploadAgg) {
	if agg.owner == "" {
		return
	}
	s.ownersMu.Lock()
	s.byOwner[agg.owner]--
	if s.byOwner[agg.owner] == 0 {
		delete(s.byOwner, agg.owner)
	}
	s.ownersMu.Unlock()
}

func (s *UploadStore) finishFor(id, owner string) uploadAccess {
	agg, ok := s.get(id)
	if !ok {
		return uploadAccessInvalid
	}
	if agg.owner != "" && owner != agg.owner {
		return uploadAccessOwnerMismatch
	}
	agg.finishOnce.Do(func() { close(agg.finished) })
	return uploadAccessOK
}

// finish marks the client-owned upload stage lifecycle as closed. Completion is
// explicit: the progress stream never guesses from a quiet gap between POSTs.
func (s *UploadStore) finish(id string) bool {
	return s.finishFor(id, "") == uploadAccessOK
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
	if agg, ok := sh.m[id]; ok {
		delete(sh.m, id)
		close(agg.expired)
		s.live.Add(-1)
		s.releaseOwner(agg)
	}
	sh.mu.Unlock()
}

// sweep reaps every aggregate idle longer than ttl. It is the store's only
// deleter of aggregate state; bytes are never lost because the authoritative read is
// the cumulative value at explicit finalization, long before idle reaping.
func (s *UploadStore) sweep(ttl time.Duration) {
	now := monoNanos()
	aggCutoff := now - int64(ttl)
	for i := range s.shards {
		sh := &s.shards[i]
		sh.mu.Lock()
		for id, agg := range sh.m {
			if agg.posts.Load() == 0 && agg.lastTouchMono.Load() < aggCutoff {
				delete(sh.m, id)
				close(agg.expired)
				s.live.Add(-1)
				s.releaseOwner(agg)
			}
		}
		sh.mu.Unlock()
	}
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
