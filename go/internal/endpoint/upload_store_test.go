package endpoint

import (
	"encoding/base64"
	"fmt"
	"strings"
	"sync"
	"testing"
	"testing/synctest"
	"time"

	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

func (s *Upload) lanesOf(a *uploadAgg) int {
	s.mu.Lock()
	defer s.mu.Unlock()
	return a.lanes
}

func (s *Upload) live() int {
	s.mu.Lock()
	defer s.mu.Unlock()
	return len(s.receivers)
}

func (s *Upload) getOrCreateFor(id, owner string) (*uploadAgg, uploadAccess) {
	return s.accessFor(id, owner, false)
}

func (s *Upload) getOrCreate(id string) (*uploadAgg, bool) {
	agg, access := s.getOrCreateFor(id, "")
	return agg, access == uploadAccessOK
}

// Only an id this store signed within the token lifetime may create state.
func TestUploadStoreRejectsForgedTamperedAndExpiredIDs(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		s := NewUpload(nil, nil)
		id := s.Mint()
		raw, err := base64.RawURLEncoding.DecodeString(id[4:])
		if err != nil {
			t.Fatal(err)
		}
		raw[len(raw)-1] ^= 1
		expiring := s.Mint()
		time.Sleep(uploadTokenTTL + time.Second)
		for name, id := range map[string]string{
			"forged":   "never-minted",
			"tampered": "gmu_" + base64.RawURLEncoding.EncodeToString(raw),
			"expired":  expiring,
			"foreign":  NewUpload(nil, nil).Mint(),
		} {
			if agg, ok := s.getOrCreate(id); ok || agg != nil {
				t.Errorf("%s id created a receiver", name)
			}
		}
		if s.live() != 0 {
			t.Fatalf("live = %d after rejected creates, want 0", s.live())
		}
		if _, ok := s.getOrCreate(s.Mint()); !ok {
			t.Fatal("a fresh id was refused")
		}
	})
}

func TestUploadStoreCreateIsIdempotent(t *testing.T) {
	s := NewUpload(nil, nil)
	id := s.Mint()
	a, ok := s.getOrCreate(id)
	if !ok || a == nil {
		t.Fatalf("first getOrCreate failed: ok=%v", ok)
	}
	b, ok := s.getOrCreate(id)
	if !ok || b != a {
		t.Fatalf("second getOrCreate returned a different aggregate (%p vs %p)", b, a)
	}
	if got, ok := s.get(id); !ok || got != a || s.live() != 1 {
		t.Fatalf("get = (%p, %v) with %d live, want the one aggregate", got, ok, s.live())
	}
}

func TestUploadStorePerOwnerCapAndOwnership(t *testing.T) {
	s := NewUpload(nil, nil)
	owner := "192.0.2.1"
	var first string
	for i := range maxLiveUploadsPerClient {
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

// Delegated owners share their subject's retention budget while keeping distinct access rights.
func TestDelegatedUploadOwnersShareTheParentRetentionBudget(t *testing.T) {
	store := NewUpload(nil, nil)
	for i := range maxLiveUploadsPerClient {
		owner := "principal:subject\x00browser-grant:first"
		if i%2 == 1 {
			owner = "principal:subject\x00browser-grant:second"
		}
		if _, access := store.getOrCreateFor(store.Mint(), owner); access != uploadAccessOK {
			t.Fatal(access)
		}
	}
	third := "principal:subject\x00browser-grant:third"
	if _, access := store.getOrCreateFor(store.Mint(), third); access != uploadAccessClientFull {
		t.Fatal("another grant multiplied retention capacity")
	}
}

// A receiver outlives a WebTransport reconnect, dies once idle past its TTL,
// never while a lane is live, and releases its owner's budget when it goes.
func TestUploadStoreSweepFollowsActivity(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		s := NewUpload(nil, nil)
		const owner = "192.0.2.1"
		carried, _ := s.getOrCreateFor(s.Mint(), owner)
		carried.recordChunk(s.now(), 1<<20)
		active, _ := s.accessFor(s.Mint(), owner, true)
		for range maxLiveUploadsPerClient - 2 {
			s.getOrCreateFor(s.Mint(), owner)
		}
		time.Sleep(wire.WTIdleBound)
		s.sweep(uploadIDTTL)
		if s.live() != maxLiveUploadsPerClient {
			t.Fatalf("live = %d within the transport's idle bound, want every receiver kept for a re-dial",
				s.live())
		}
		if _, access := s.getOrCreateFor(s.Mint(), owner); access != uploadAccessClientFull {
			t.Fatalf("owner at its cap created another receiver: %v", access)
		}
		time.Sleep(uploadIDTTL)
		s.sweep(uploadIDTTL)
		if s.live() != 1 {
			t.Fatalf("live = %d past the TTL, want only the receiver with a live lane", s.live())
		}
		if _, access := s.getOrCreateFor(s.Mint(), owner); access != uploadAccessOK {
			t.Fatalf("reaped receivers kept their owner's capacity: %v", access)
		}
		s.leave(active)
		time.Sleep(uploadIDTTL + time.Second)
		s.sweep(uploadIDTTL)
		if s.live() != 0 {
			t.Fatalf("live = %d after the last lane ended and the TTL passed, want 0", s.live())
		}
	})
}

// A finished receiver keeps its completion and owner for as long as its token could recreate state.
func TestFinishedUploadKeepsOwnershipUntilTokenExpires(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		store := NewUpload(nil, nil)
		id := store.Mint()
		agg, _ := store.getOrCreateFor(id, "original")
		if access := store.finishFor(id, "original"); access != uploadAccessOK {
			t.Fatal(access)
		}
		time.Sleep(uploadTokenTTL - time.Second)
		store.sweep(uploadIDTTL)
		if retained, ok := store.get(id); !ok || retained != agg {
			t.Fatal("finished receiver was swept while its token could still create state")
		}
		if _, access := store.accessFor(id, "original", true); access != uploadAccessInvalid {
			t.Fatalf("finished owner rejoined with access %v", access)
		}
		if _, access := store.accessFor(id, "other", true); access != uploadAccessOwnerMismatch {
			t.Fatalf("ownership was lost with access %v", access)
		}
	})
}

// At the global cap a new receiver displaces the oldest one that never received a byte, so clients that only
// open feeds cannot hold the cap; receivers with bytes or a finish are never displaced.
func TestUploadStoreCapDisplacesOnlyEmptyReceivers(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		s := NewUpload(nil, nil)
		fill := func(keep func(*uploadAgg)) []*uploadAgg {
			var aggs []*uploadAgg
			for i := range maxLiveUploads - int(s.live()) {
				id := s.Mint()
				agg, access := s.getOrCreateFor(id, fmt.Sprint("watcher-", i%50))
				if access != uploadAccessOK {
					t.Fatalf("filler %d = %v", i, access)
				}
				keep(agg)
				aggs = append(aggs, agg)
				time.Sleep(time.Millisecond)
			}
			return aggs
		}
		watchers := fill(func(*uploadAgg) {})
		upload := s
		if n, err := upload.Receive(t.Context(), s.Mint(), "client", strings.NewReader("lane")); err != nil || n != 4 {
			t.Fatalf("lane at a cap of empty receivers = %d, %v", n, err)
		}
		select {
		case <-watchers[0].expired:
		default:
			t.Fatal("the oldest empty receiver was not the one displaced")
		}
		for _, agg := range watchers[2:] {
			agg.recordChunk(s.now(), 1)
		}
		s.mu.Lock()
		close(watchers[1].finished)
		s.mu.Unlock()
		if _, err := upload.Receive(t.Context(), s.Mint(), "client", strings.NewReader("lane")); err == nil {
			t.Fatal("a receiver holding bytes or a finish was displaced")
		}
		select {
		case <-watchers[1].expired:
			t.Fatal("a finished receiver was displaced while its token could still recreate it")
		default:
		}
	})
}

// fillStore holds the global cap with receivers that each accepted a byte.
func fillStore(s *Upload) {
	for range maxLiveUploads - int(s.live()) {
		agg, _ := s.getOrCreate(s.Mint())
		agg.recordChunk(s.now(), 1)
	}
}

func TestUploadStoreCapAllowsCreateAfterSweepFreesSpace(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		s := NewUpload(nil, nil)
		agg, _ := s.getOrCreate(s.Mint())
		agg.recordChunk(s.now(), 1)
		time.Sleep(uploadIDTTL + time.Second)
		fillStore(s)
		blocked := s.Mint()
		if _, ok := s.getOrCreate(blocked); ok {
			t.Fatal("create at the cap unexpectedly succeeded")
		}
		s.sweep(uploadIDTTL)
		if _, ok := s.getOrCreate(blocked); !ok || s.live() != maxLiveUploads {
			t.Fatalf("create after the sweep freed a slot = %v with %d live, want it admitted", ok, s.live())
		}
	})
}

func TestUploadStoreConcurrentGetAndSweep(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		s := NewUpload(nil, nil)
		const n = 200
		ids := make([]string, n)
		for i := range ids {
			// Half are already past the TTL, so sweeping reaps them while readers are still hammering the store.
			if i == n/2 {
				time.Sleep(time.Second)
			}
			ids[i] = s.Mint()
			s.getOrCreate(ids[i])
		}
		var wg sync.WaitGroup
		stop := make(chan struct{})
		wg.Go(func() {
			for {
				select {
				case <-stop:
					return
				default:
					s.sweep(500 * time.Millisecond)
				}
			}
		})
		var readers sync.WaitGroup
		for range 4 {
			readers.Go(func() {
				for i := range 500 {
					s.getOrCreate(ids[i%n])
				}
			})
		}
		readers.Wait()
		close(stop)
		wg.Wait()
		if live := s.live(); live > n {
			t.Fatalf("live = %d receivers from %d ids", live, n)
		}
	})
}

// Elapsed receiver time is anchored at the first chunk, never moves, and includes stalls.
func TestUploadAggElapsedTimeIsAnchoredAtTheFirstChunk(t *testing.T) {
	var a uploadAgg
	const ms = int64(time.Millisecond)
	if got := a.elapsedNanos(1000 * ms); got != 0 {
		t.Fatalf("elapsed before any chunk = %d, want 0", got)
	}
	a.recordChunk(1000*ms, 100)
	a.recordChunk(500*ms, 100) // a late-stamped chunk does not move the anchor
	if got := a.elapsedNanos(1050 * ms); got != 50*ms {
		t.Fatalf("elapsed after 50ms = %d, want %d", got, 50*ms)
	}
	// No chunk arrives during this 2s stall, but it remains in the time.
	a.recordChunk(3060*ms, 100)
	if got := a.elapsedNanos(3060 * ms); got != 2060*ms || a.bytes.Load() != 300 {
		t.Fatalf("elapsed %d with %d bytes after a stall, want %d with 300", got, a.bytes.Load(), 2060*ms)
	}
}
