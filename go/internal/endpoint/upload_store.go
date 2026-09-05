package endpoint

import (
	"context"
	"crypto/hmac"
	"crypto/rand"
	"crypto/sha256"
	"encoding/base64"
	"encoding/binary"
	"hash/fnv"
	"maps"
	"slices"
	"sync"
	"sync/atomic"
	"time"

	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

// UploadStore holds per-test state shared between POST /upload lanes and the /upload/progress stream.
type UploadStore struct {
	shards   [uploadShardCount]uploadShard
	tokenKey func() ([sha256.Size]byte, bool)
	live     atomic.Int32 // live aggregates retained through completion replay
	ownersMu sync.Mutex
	byOwner  map[string]int
}

type uploadShard struct {
	mu sync.Mutex
	m  map[string]*uploadAgg
}

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

func (a *uploadAgg) postsWaiter() <-chan struct{} {
	a.postsMu.Lock()
	defer a.postsMu.Unlock()
	if a.postsChanged == nil {
		a.postsChanged = make(chan struct{})
	}
	return a.postsChanged
}

func (a *uploadAgg) claimProgress() chan struct{} {
	a.progressMu.Lock()
	defer a.progressMu.Unlock()
	if a.progressHeld != nil {
		close(a.progressHeld)
	}
	a.progressHeld = make(chan struct{})
	return a.progressHeld
}

func (a *uploadAgg) releaseProgress(claim chan struct{}) {
	a.progressMu.Lock()
	defer a.progressMu.Unlock()
	if a.progressHeld == claim {
		a.progressHeld = nil
	}
}

func (a *uploadAgg) recordChunk(now int64, n int) {
	a.firstChunkMono.CompareAndSwap(0, now)
	a.bytes.Add(int64(n))
	a.lastTouchMono.Store(now) // keeps the id from looking idle to the sweeper
}

func (a *uploadAgg) changePosts(delta int32) {
	a.posts.Add(delta)
	a.postsMu.Lock()
	defer a.postsMu.Unlock()
	if a.postsChanged != nil {
		close(a.postsChanged)
		a.postsChanged = nil
	}
}

func (a *uploadAgg) elapsedNanos(now int64) int64 {
	start := a.firstChunkMono.Load()
	if start == 0 || now <= start {
		return 0
	}
	return now - start
}

const (
	uploadShardCount        = 32
	maxLiveUploads          = 1000
	maxLiveUploadsPerClient = 32
	uploadReconnectGrace    = 30 * time.Second
	uploadIDTTL             = 2*wire.WTIdleBound + uploadReconnectGrace
	uploadTokenTTL          = 2 * time.Minute
	uploadSweepInterval     = 5 * time.Second
)

// NewUploadStore builds an empty store with its shard maps initialised.
func NewUploadStore() *UploadStore {
	s := &UploadStore{
		byOwner: make(map[string]int),
		tokenKey: sync.OnceValues(func() ([sha256.Size]byte, bool) {
			var key [sha256.Size]byte
			_, err := rand.Read(key[:])
			return key, err == nil
		}),
	}
	for i := range s.shards {
		s.shards[i].m = make(map[string]*uploadAgg)
	}
	return s
}

var uploadMonoOrigin = time.Now()

func monoNanos() int64 { return int64(time.Since(uploadMonoOrigin)) }

func (s *UploadStore) shard(id string) *uploadShard {
	h := fnv.New32a()
	_, _ = h.Write([]byte(id))
	return &s.shards[h.Sum32()%uploadShardCount]
}

// Mint generates a URL-safe, authenticated upload-session token without storing per-token state.
func (s *UploadStore) Mint() string {
	key, ok := s.tokenKey()
	if !ok {
		return ""
	}
	var nonce [16]byte
	if _, err := rand.Read(nonce[:]); err != nil {
		return ""
	}
	return s.signID(monoNanos(), nonce, key)
}

func (s *UploadStore) signID(issued int64, nonce [16]byte, key [sha256.Size]byte) string {
	var payload [8 + len(nonce)]byte
	binary.BigEndian.PutUint64(payload[:8], uint64(issued)) //nosec G115 -- issued is a positive monotonic-nanos timestamp
	copy(payload[8:], nonce[:])
	mac := hmac.New(sha256.New, key[:])
	_, _ = mac.Write(payload[:])
	return "gmu_" + base64.RawURLEncoding.EncodeToString(slices.Concat(payload[:], mac.Sum(nil)))
}

func (s *UploadStore) validID(id string) bool {
	key, ok := s.tokenKey()
	if len(id) < 4 || id[:4] != "gmu_" || !ok {
		return false
	}
	raw, err := base64.RawURLEncoding.DecodeString(id[4:])
	if err != nil || len(raw) != 8+16+sha256.Size {
		return false
	}
	payload, tag := raw[:24], raw[24:]
	mac := hmac.New(sha256.New, key[:])
	_, _ = mac.Write(payload)
	if !hmac.Equal(tag, mac.Sum(nil)) {
		return false
	}
	issued := int64(binary.BigEndian.Uint64(payload[:8])) //nosec G115 -- round-trips the value signID wrote
	now := monoNanos()
	return issued > 0 && issued <= now && now-issued <= int64(uploadTokenTTL)
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
	defer sh.mu.Unlock()
	if agg, ok := sh.m[id]; ok {
		if agg.owner != "" && owner != agg.owner {
			return nil, uploadAccessOwnerMismatch
		}
		if touch {
			agg.lastTouchMono.Store(monoNanos())
		}
		return agg, uploadAccessOK
	}
	if !s.validID(id) {
		return nil, uploadAccessInvalid
	}
	for {
		n := s.live.Load()
		if n >= maxLiveUploads {
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
			return nil, uploadAccessClientFull
		}
		s.byOwner[owner]++
		s.ownersMu.Unlock()
	}
	agg := &uploadAgg{finished: make(chan struct{}), expired: make(chan struct{}), owner: owner}
	agg.lastTouchMono.Store(monoNanos())
	sh.m[id] = agg
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

// finishFor marks id's upload complete on behalf of owner, releasing the progress stream to emit its terminal record.
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

func (s *UploadStore) get(id string) (*uploadAgg, bool) {
	if id == "" {
		return nil, false
	}
	sh := s.shard(id)
	sh.mu.Lock()
	defer sh.mu.Unlock()
	agg, ok := sh.m[id]
	return agg, ok
}

func (s *UploadStore) expire(agg *uploadAgg) {
	close(agg.expired)
	s.live.Add(-1)
	s.releaseOwner(agg)
}

func (s *UploadStore) sweep(ttl time.Duration) {
	now := monoNanos()
	aggCutoff := now - int64(ttl)
	for i := range s.shards {
		sh := &s.shards[i]
		sh.mu.Lock()
		maps.DeleteFunc(sh.m, func(_ string, agg *uploadAgg) bool {
			if agg.posts.Load() != 0 || agg.lastTouchMono.Load() >= aggCutoff {
				return false
			}
			s.expire(agg)
			return true
		})
		sh.mu.Unlock()
	}
}

// RunSweeper reaps idle aggregates until ctx is cancelled.
func (s *UploadStore) RunSweeper(ctx context.Context) {
	ticker := time.Tick(uploadSweepInterval)
	for {
		select {
		case <-ctx.Done():
			return
		case <-ticker:
			s.sweep(uploadIDTTL)
		}
	}
}
