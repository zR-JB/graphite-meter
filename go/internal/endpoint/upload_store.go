package endpoint

import (
	"context"
	"crypto/hmac"
	"crypto/rand"
	"crypto/sha256"
	"encoding/base64"
	"encoding/binary"
	"maps"
	"net/http"
	"slices"
	"strings"
	"sync/atomic"
	"time"

	"github.com/zR-JB/graphite-meter/go/internal/transport"
	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

// uploadAgg is one receiver. Its counters are atomic; the rest is guarded by the Upload's mu.
type uploadAgg struct {
	bytes          atomic.Int64 // drained bytes across all of this id's lanes
	firstChunkMono atomic.Int64 // mono ns of the first drained chunk; set exactly once
	lastTouch      int64        // mono ns a lane last joined or left: the sweeper's idle clock
	client         uploadClient
	lanes          int
	lanesChanged   chan struct{} // closed and replaced on every change: a broadcast
	finished       chan struct{} // closed by DELETE /upload/progress
	expired        chan struct{} // closed when idle state is reaped
	feed           chan struct{} // closed when a later feed supersedes the holder
}

func (a *uploadAgg) recordChunk(now int64, n int) {
	if a.firstChunkMono.Load() == 0 {
		a.firstChunkMono.CompareAndSwap(0, now)
	}
	a.bytes.Add(int64(n))
}

func (a *uploadAgg) isFinished() bool {
	select {
	case <-a.finished:
		return true
	default:
		return false
	}
}

func (a *uploadAgg) setLanesLocked(n int) {
	a.lanes = n
	if a.lanesChanged != nil {
		close(a.lanesChanged)
		a.lanesChanged = nil
	}
}

func (u *Upload) leave(a *uploadAgg) {
	u.mu.Lock()
	defer u.mu.Unlock()
	a.setLanesLocked(a.lanes - 1)
	a.lastTouch = u.now()
}

func (u *Upload) claimFeed(a *uploadAgg) chan struct{} {
	u.mu.Lock()
	defer u.mu.Unlock()
	if a.feed != nil {
		close(a.feed)
	}
	a.feed = make(chan struct{})
	return a.feed
}

func (u *Upload) releaseFeed(a *uploadAgg, claim chan struct{}) {
	u.mu.Lock()
	defer u.mu.Unlock()
	if a.feed == claim {
		a.feed = nil
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
	maxLiveUploads          = 1000
	maxLiveUploadsPerClient = 32
	uploadReconnectGrace    = 30 * time.Second
	uploadTokenTTL          = 2 * time.Minute
	// A retained ID outlives its signed token, so sweeping cannot forget a live token's state.
	uploadIDTTL         = max(2*wire.WTIdleBound+uploadReconnectGrace, uploadTokenTTL)
	uploadSweepInterval = 5 * time.Second
)

// mono is the monotonic receiver clock in ns; it starts at 1 so zero marks an unset anchor.
func (u *Upload) mono(t time.Time) int64 { return int64(t.Sub(u.epoch)) + 1 }

func (u *Upload) now() int64 { return u.mono(time.Now()) }

func (u *Upload) Mint() string {
	var payload [8 + 16]byte
	binary.BigEndian.PutUint64(payload[:8], uint64(u.now())) //nosec G115 -- a positive monotonic timestamp
	_, _ = rand.Read(payload[8:])
	mac := hmac.New(sha256.New, u.tokenKey[:])
	_, _ = mac.Write(payload[:])
	return "gmu_" + base64.RawURLEncoding.EncodeToString(slices.Concat(payload[:], mac.Sum(nil)))
}

func (u *Upload) validID(id string) bool {
	encoded, ok := strings.CutPrefix(id, "gmu_")
	if !ok {
		return false
	}
	raw, err := base64.RawURLEncoding.DecodeString(encoded)
	if err != nil || len(raw) != 8+16+sha256.Size {
		return false
	}
	payload, tag := raw[:24], raw[24:]
	mac := hmac.New(sha256.New, u.tokenKey[:])
	_, _ = mac.Write(payload)
	if !hmac.Equal(tag, mac.Sum(nil)) {
		return false
	}
	issued := int64(binary.BigEndian.Uint64(payload[:8])) //nosec G115 -- round-trips the value Mint wrote
	now := u.now()
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

var uploadAccessInfos = [...]struct {
	message, code string
	status        int
}{
	uploadAccessOK:            {},
	uploadAccessInvalid:       {"unknown upload id", "invalid", http.StatusBadRequest},
	uploadAccessGlobalFull:    {"upload capacity exhausted", "globalFull", http.StatusServiceUnavailable},
	uploadAccessClientFull:    {"client upload capacity exhausted", "clientFull", http.StatusTooManyRequests},
	uploadAccessOwnerMismatch: {"upload id belongs to another client", "ownerMismatch", http.StatusForbidden},
}

// uploadRefusalError carries the classified refusal across transports that have no HTTP status line.
type uploadRefusalError struct{ access uploadAccess }

func (e *uploadRefusalError) Error() string {
	return "upload refused: " + uploadAccessInfos[e.access].message
}

func writeUploadAccessError(w http.ResponseWriter, access uploadAccess) {
	info := uploadAccessInfos[access]
	w.Header().Set("X-Graphite-Upload-Refusal", info.code)
	if access == uploadAccessGlobalFull || access == uploadAccessClientFull {
		w.Header().Set("Retry-After", "1")
	}
	http.Error(w, info.message, info.status)
}

// accessFor resolves or creates id's receiver; lanes join under the lock, watchers stay passive.
func (u *Upload) accessFor(id string, c uploadClient, join bool) (*uploadAgg, uploadAccess) {
	u.mu.Lock()
	defer u.mu.Unlock()
	agg, ok := u.receivers[id]
	if c.owner == "" || ok && c.owner != agg.client.owner {
		return nil, uploadAccessOwnerMismatch
	}
	if ok {
		if join {
			if agg.isFinished() {
				return nil, uploadAccessInvalid
			}
			agg.setLanesLocked(agg.lanes + 1)
			agg.lastTouch = u.now()
		}
		return agg, uploadAccessOK
	}
	if _, evicted := u.evicted[id]; evicted || !u.validID(id) {
		return nil, uploadAccessInvalid
	}
	if transport.ShareFull(c.keys, maxLiveUploadsPerClient, func(key string) int { return u.byClient[key] }) {
		return nil, uploadAccessClientFull
	}
	if len(u.receivers) >= maxLiveUploads && !u.evictEmptyLocked() {
		return nil, uploadAccessGlobalFull
	}
	for _, key := range c.keys {
		u.byClient[key]++
	}
	agg = &uploadAgg{finished: make(chan struct{}), expired: make(chan struct{}), client: c}
	agg.lastTouch = u.now()
	u.receivers[id] = agg
	if join {
		agg.lanes = 1
	}
	return agg, uploadAccessOK
}

// evictEmptyLocked expires the stalest receiver without bytes, lanes or finish; its id stays refused.
func (u *Upload) evictEmptyLocked() bool {
	if len(u.evicted) >= maxLiveUploads {
		return false
	}
	victim := ""
	var oldest int64
	for id, agg := range u.receivers {
		if agg.lanes == 0 && agg.bytes.Load() == 0 && !agg.isFinished() && (victim == "" || agg.lastTouch < oldest) {
			victim, oldest = id, agg.lastTouch
		}
	}
	if victim != "" {
		u.expireLocked(victim)
		u.evicted[victim] = u.now() + int64(uploadTokenTTL)
	}
	return victim != ""
}

func (u *Upload) expireLocked(id string) {
	agg := u.receivers[id]
	delete(u.receivers, id)
	close(agg.expired)
	for _, key := range agg.client.keys {
		if u.byClient[key]--; u.byClient[key] == 0 {
			delete(u.byClient, key)
		}
	}
}

func (u *Upload) finishFor(id, owner string) uploadAccess {
	u.mu.Lock()
	defer u.mu.Unlock()
	agg, ok := u.receivers[id]
	switch {
	case !ok:
		return uploadAccessInvalid
	case owner != agg.client.owner:
		return uploadAccessOwnerMismatch
	case !agg.isFinished():
		close(agg.finished)
	}
	return uploadAccessOK
}

func (u *Upload) get(id string) (*uploadAgg, bool) {
	u.mu.Lock()
	defer u.mu.Unlock()
	agg, ok := u.receivers[id]
	return agg, ok
}

func (u *Upload) sweep(ttl time.Duration) {
	now := u.now()
	cutoff := now - int64(ttl)
	u.mu.Lock()
	defer u.mu.Unlock()
	maps.DeleteFunc(u.evicted, func(_ string, until int64) bool { return until < now })
	for id, agg := range u.receivers {
		if agg.lanes == 0 && agg.lastTouch < cutoff {
			u.expireLocked(id)
		}
	}
}

func (u *Upload) RunSweeper(ctx context.Context) {
	ticker := time.Tick(uploadSweepInterval)
	for {
		select {
		case <-ctx.Done():
			return
		case <-ticker:
			u.sweep(uploadIDTTL)
		}
	}
}
