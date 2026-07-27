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

	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

// UploadStore holds per-test state shared between POST /upload lanes and the
// /upload/progress stream, separate requests joined by ?id=, so the SERVER's
// drained byte count is authoritative. Creating an aggregate requires a
// short-lived authenticated id from /upload/session; minting is stateless.
type UploadStore struct {
	shards       [uploadShardCount]uploadShard
	tokenKey     [sha256.Size]byte
	tokenKeyOnce sync.Once
	tokenKeyOK   bool
	live         atomic.Int32 // live aggregates retained through completion replay
	ownersMu     sync.Mutex
	byOwner      map[string]int
}

// uploadShard is one lock's worth of the aggregate map. Only first-touch create
// and sweep delete take the mutex; a POST's byte add is a lockless atomic.
type uploadShard struct {
	mu sync.Mutex
	m  map[string]*uploadAgg
}

// uploadAgg is one test's cross-connection accumulator. Every field is atomic, so
// the POST drain path (recordChunk) and the NDJSON stream (bytes.Load every
// 100 ms) never share a lock.
type uploadAgg struct {
	bytes          atomic.Int64 // cumulative drained bytes across ALL this id's POST lanes
	firstChunkMono atomic.Int64 // mono ns of the first drained chunk; set exactly once
	lastTouchMono  atomic.Int64 // mono ns of the last drained chunk; the sweeper's idle clock
	posts          atomic.Int32 // live POST lanes for this id (diagnostics; NOT a deleter)
	postsMu        sync.Mutex
	postsChanged   chan struct{} // closed and replaced on every change: a broadcast
	finished       chan struct{} // explicitly closed by DELETE /upload/progress
	expired        chan struct{} // closed when idle state is reaped
	finishOnce     sync.Once
	progressMu     sync.Mutex
	progressHeld   chan struct{} // closed when a later claim supersedes the holder
	owner          string
}

// postsWaiter returns a channel closed by the next lane-count change. A caller
// takes it BEFORE reading posts, so a change racing that read still wakes it.
func (a *uploadAgg) postsWaiter() <-chan struct{} {
	a.postsMu.Lock()
	defer a.postsMu.Unlock()
	if a.postsChanged == nil {
		a.postsChanged = make(chan struct{})
	}
	return a.postsChanged
}

// claimProgress makes the caller the aggregate's one live feed, superseding any
// current holder. A client that lost its transport re-dials long before the
// dead connection's idle timeout, so the newest feed always wins. Both feeds
// have passed the owner check, but that check is deliberately coarse: ClientKey
// groups by authenticated SUBJECT, not by login, so the same user's second
// device takes over the feed, and in public mode it groups by address (IPv6 by
// /64), so a shared address does too. The unguessable minted id is what actually
// gates the aggregate; the owner check only bounds per-client capacity.
func (a *uploadAgg) claimProgress() chan struct{} {
	a.progressMu.Lock()
	defer a.progressMu.Unlock()
	if a.progressHeld != nil {
		close(a.progressHeld)
	}
	a.progressHeld = make(chan struct{})
	return a.progressHeld
}

// releaseProgress clears the claim unless a later feed already superseded it.
func (a *uploadAgg) releaseProgress(claim chan struct{}) {
	a.progressMu.Lock()
	defer a.progressMu.Unlock()
	if a.progressHeld == claim {
		a.progressHeld = nil
	}
}

// recordChunk counts one drained chunk and starts the elapsed clock on the first
// byte. CompareAndSwap makes the anchor safe across parallel POST lanes.
func (a *uploadAgg) recordChunk(now int64, n int) {
	a.firstChunkMono.CompareAndSwap(0, now)
	a.bytes.Add(int64(n))
	a.lastTouchMono.Store(now) // keeps the id from looking idle to the sweeper
}

// changePosts adjusts the live lane count and wakes every waiter. A feed being
// superseded can share the wait, and a single-token nudge would wake only one
// of them, so the notification is a broadcast: close the current channel and
// let the next waiter install a fresh one.
func (a *uploadAgg) changePosts(delta int32) {
	a.posts.Add(delta)
	a.postsMu.Lock()
	defer a.postsMu.Unlock()
	if a.postsChanged != nil {
		close(a.postsChanged)
		a.postsChanged = nil
	}
}

// elapsedNanos returns the rate denominator: time since the first drained chunk.
// That anchor excludes warmup and keeps every measured pause. Congestion, lane
// turnaround, reconnects and stalls must reduce throughput, not vanish.
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
	// uploadReconnectGrace is the budget a client gets, on top of the transport
	// bound, to notice a bound-driven close and re-dial against the same id.
	uploadReconnectGrace = 30 * time.Second
	// uploadIDTTL: an aggregate idle this long is reaped after an abort, tab
	// close, or crash. It MUST outlast a session's whole death — watchSession
	// cancels on the second quiet tick, so a stalled session survives up to 1.5
	// bounds — or the re-dial takes the create path and restarts the count at
	// zero. The progress feed does not touch the clock, so watching cannot
	// stretch it.
	uploadIDTTL = 2*wire.WTIdleBound + uploadReconnectGrace
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

// monoNanos is the store's monotonic clock: ns since process start (startMono
// lives in ping.go, same package). time.Since reads the monotonic clock, so the
// value never jumps with wall-clock changes, safe for idle-age comparisons.
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
	binary.BigEndian.PutUint64(payload[:8], uint64(issued)) //nosec G115 -- issued is a positive monotonic-nanos timestamp
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
	issued := int64(binary.BigEndian.Uint64(payload[:8])) //nosec G115 -- round-trips the value signID wrote
	now := monoNanos()
	return issued > 0 && issued <= now && now-issued <= int64(uploadTokenTTL)
}

// getOrCreate is getOrCreateFor with no owner, collapsing the access reason to a
// plain ok.
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

// getOrCreateFor is getOrCreateForActivity for a caller that is actually moving
// upload bytes, so the touch refreshes the sweeper's idle clock.
func (s *UploadStore) getOrCreateFor(id, owner string) (*uploadAgg, uploadAccess) {
	return s.getOrCreateForActivity(id, owner, true)
}

// getOrCreateForActivity returns id's aggregate, creating it on first touch and
// attributing it to owner. Id authentication and both caps apply only to that
// first create, so an expiring token or a filled cap never strands a running
// test's later lanes. touch refreshes the sweeper's idle clock.
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
		// Touch under the shard lock the sweeper also takes, so an aggregate
		// cannot be reaped between the lookup that found it and the refresh.
		if touch {
			agg.lastTouchMono.Store(monoNanos())
		}
		sh.mu.Unlock()
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
	agg := &uploadAgg{finished: make(chan struct{}), expired: make(chan struct{}), owner: owner}
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

// finishFor marks id's upload complete on behalf of owner, releasing the progress
// stream to emit its terminal record. Idempotent, so a retried DELETE is safe;
// the aggregate itself lives on until the sweeper reaps it, which is what lets a
// reconnecting client replay the completion.
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

// delete removes id's aggregate if present. Idempotent: a second delete, or one
// racing the sweeper, is a no-op and never double-decrements `live`.
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
// deleter of aggregate state. No bytes are lost: the authoritative read is the
// cumulative value at explicit finalization, which long precedes idle reaping.
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
