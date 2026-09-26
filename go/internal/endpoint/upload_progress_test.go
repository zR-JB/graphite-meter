package endpoint

import (
	"bytes"
	"context"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync"
	"testing"
	"testing/synctest"
	"time"

	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

// progressRecorder is a flushable, race-safe ResponseWriter.
type progressRecorder struct {
	mu     sync.Mutex
	header http.Header
	body   bytes.Buffer
}

func newProgressRecorder() *progressRecorder {
	return &progressRecorder{header: make(http.Header)}
}

func (r *progressRecorder) Header() http.Header { return r.header }
func (r *progressRecorder) WriteHeader(int)     {}
func (r *progressRecorder) Flush()              {}
func (r *progressRecorder) Write(p []byte) (int, error) {
	r.mu.Lock()
	defer r.mu.Unlock()
	return r.body.Write(p)
}

func (r *progressRecorder) text() string {
	r.mu.Lock()
	defer r.mu.Unlock()
	return r.body.String()
}

// startFeed serves one progress GET in the bubble; the returned channel closes when the feed ends.
func startFeed(ctx context.Context, h http.Handler, id string) (*progressRecorder, <-chan struct{}) {
	rec, done := newProgressRecorder(), make(chan struct{})
	go func() {
		defer close(done)
		h.ServeHTTP(rec, httptest.NewRequest(http.MethodGet, "/upload/progress?id="+id, nil).WithContext(ctx))
	}()
	synctest.Wait()
	return rec, done
}

func ended(done <-chan struct{}) bool {
	select {
	case <-done:
		return true
	default:
		return false
	}
}

func TestUploadProgressNDJSONLifecycle(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		store := NewUpload(nil, nil)
		id := store.Mint()
		h := http.HandlerFunc(store.ServeProgress)
		rec, done := startFeed(t.Context(), h, id)
		if !strings.Contains(rec.text(), `{"type":"ready"}`) {
			t.Fatalf("feed opened with %q, want ready", rec.text())
		}
		if rec.Header().Get("Content-Type") != "application/x-ndjson" ||
			rec.Header().Get("Cache-Control") != "no-store, no-transform" {
			t.Fatalf("feed headers = %v", rec.Header())
		}
		agg, access := store.accessFor(id, ownedBy("192.0.2.1"), true)
		if access != uploadAccessOK {
			t.Fatal("aggregate not created by progress GET")
		}
		agg.recordChunk(store.now(), 4096)
		time.Sleep(uploadProgressTick)
		synctest.Wait()
		if want := `{"type":"progress","bytes":4096,"nanos":100000000}`; !strings.Contains(rec.text(), want) {
			t.Fatalf("feed = %s, want %s", rec.text(), want)
		}

		finish := httptest.NewRecorder()
		h.ServeHTTP(finish, httptest.NewRequest(http.MethodDelete, "/upload/progress?id="+id, nil))
		if finish.Code != http.StatusNoContent {
			t.Fatalf("DELETE status = %d, want %d", finish.Code, http.StatusNoContent)
		}
		// The terminal record waits for the lane still in flight.
		synctest.Wait()
		if ended(done) {
			t.Fatal("the feed completed while a lane was still delivering")
		}
		store.leave(agg)
		synctest.Wait()
		if !ended(done) || !strings.Contains(rec.text(), `"type":"complete","bytes":4096`) {
			t.Fatalf("feed after the last lane = %s, want a terminal complete record", rec.text())
		}

		// Completion is replayable until the aggregate TTL expires.
		replay := httptest.NewRecorder()
		h.ServeHTTP(replay, httptest.NewRequest(http.MethodGet, "/upload/progress?id="+id, nil))
		if want := `"type":"complete","bytes":4096`; !strings.Contains(replay.Body.String(), want) {
			t.Fatalf("replayed body = %s, want it to contain %q", replay.Body.String(), want)
		}
	})
}

// A reconnecting client re-dials long before its dead transport's idle timeout releases the old feed.
func TestUploadProgressNewFeedSupersedesOldHolder(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		store := NewUpload(nil, nil)
		id := store.Mint()
		h := http.HandlerFunc(store.ServeProgress)
		_, first := startFeed(t.Context(), h, id)
		_, second := startFeed(t.Context(), h, id)
		if !ended(first) {
			t.Fatal("the second feed did not supersede the first")
		}
		// The first feed's release has run; it must not have dropped the second's live claim.
		ctx, cancel := context.WithCancel(t.Context())
		takeover, third := startFeed(ctx, h, id)
		if !ended(second) || !strings.Contains(takeover.text(), `{"type":"ready"}`) {
			t.Fatalf("superseded feed ended = %v, takeover = %q", ended(second), takeover.text())
		}
		cancel()
		<-third
	})
}

// A superseded feed must abandon the terminal wait rather than sit on it until its transport dies.
func TestSupersededFeedLeavesTheTerminalWait(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		store := NewUpload(nil, nil)
		agg, _ := store.accessFor(store.Mint(), ownedBy("192.0.2.1"), true)
		superseded := make(chan struct{})
		done := make(chan bool, 1)
		go func() { done <- store.waitDrained(make(chan struct{}), superseded, agg) }()
		close(superseded)
		synctest.Wait()
		select {
		case ok := <-done:
			if ok {
				t.Fatal("superseded feed reported a drained count, want an abandoned wait")
			}
		default:
			t.Fatal("superseded feed stayed in the terminal wait")
		}
	})
}

// Every tick reports receiver time whether or not bytes moved, so zero delivery differs from a missing feed.
func TestProgressReportsReceiverTimeForZeroDelivery(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		store := NewUpload(nil, nil)
		agg := &uploadAgg{finished: make(chan struct{}), expired: make(chan struct{})}
		agg.recordChunk(store.now(), 4096)
		done := make(chan struct{})
		var mu sync.Mutex
		var records []wire.UploadProgress
		go store.runProgress(done, make(chan struct{}), agg, func(e wire.UploadProgress) bool {
			mu.Lock()
			defer mu.Unlock()
			records = append(records, e)
			return true
		}, func() bool { return true })
		time.Sleep(5*uploadProgressTick + uploadProgressTick/2)
		synctest.Wait()
		close(done)
		mu.Lock()
		defer mu.Unlock()
		if len(records) != 5 {
			t.Fatalf("%d records over five ticks, want 5: %+v", len(records), records)
		}
		for i, e := range records {
			want := uint64(i+1) * uint64(uploadProgressTick)
			if e.Type != "progress" || e.Bytes != 4096 || e.Nanos != want {
				t.Fatalf("record %d = %+v, want progress of 4096 bytes at %d ns", i, e, want)
			}
		}
	})
}

func TestUploadProgressRefusalResponses(t *testing.T) {
	// A feed that wrongly opens ends with the request instead of holding the test to its timeout.
	serve := func(store *Upload, method, id, remote string) *httptest.ResponseRecorder {
		ctx, cancel := context.WithTimeout(t.Context(), 5*time.Second)
		defer cancel()
		req := httptest.NewRequestWithContext(ctx, method, "/upload/progress?id="+id, nil)
		if remote != "" {
			req.RemoteAddr = remote + ":1234"
		}
		rec := httptest.NewRecorder()
		store.ServeProgress(rec, req)
		return rec
	}
	t.Run("an unknown id is a 400", func(t *testing.T) {
		for _, method := range []string{http.MethodGet, http.MethodDelete} {
			rec := serve(NewUpload(nil, nil), method, "forged", "")
			if rec.Code != http.StatusBadRequest || !strings.Contains(rec.Body.String(), "unknown upload id") {
				t.Fatalf("%s = %d %q, want 400 naming the unknown id", method, rec.Code, rec.Body.String())
			}
		}
	})

	t.Run("client cap is a retryable 429", func(t *testing.T) {
		store := NewUpload(nil, nil)
		const owner = "192.0.2.1"
		for i := range maxLiveUploadsPerClient {
			if _, access := store.getOrCreateFor(store.Mint(), owner); access != uploadAccessOK {
				t.Fatalf("filler create %d below the per-owner cap = %v", i, access)
			}
		}
		rec := serve(store, http.MethodGet, store.Mint(), owner)
		if rec.Code != http.StatusTooManyRequests || rec.Header().Get("Retry-After") != "1" {
			t.Fatalf("status = %d Retry-After %q, want a retryable 429", rec.Code, rec.Header().Get("Retry-After"))
		}
	})

	t.Run("another client's id is a 403", func(t *testing.T) {
		store := NewUpload(nil, nil)
		id := store.Mint()
		if _, access := store.getOrCreateFor(id, "192.0.2.1"); access != uploadAccessOK {
			t.Fatalf("create = %v", access)
		}
		for _, method := range []string{http.MethodGet, http.MethodDelete} {
			if rec := serve(store, method, id, "192.0.2.2"); rec.Code != http.StatusForbidden {
				t.Fatalf("%s = %d body %q: another client reached the upload", method, rec.Code, rec.Body.String())
			}
		}
		if agg, _ := store.get(id); agg.isFinished() {
			t.Fatal("another client finished the upload")
		}
	})

	// GET's route also carries HEAD, which must neither create a receiver nor claim its feed.
	t.Run("HEAD is a 405", func(t *testing.T) {
		store := NewUpload(nil, nil)
		rec := serve(store, http.MethodHead, store.Mint(), "")
		if rec.Code != http.StatusMethodNotAllowed || store.live() != 0 {
			t.Fatalf("status = %d with %d receivers, want 405 and none", rec.Code, store.live())
		}
	})
}

// Watching is not upload activity: an untouched receiver is reaped at its TTL and its feed ends with it.
func TestUploadProgressDoesNotRefreshAggregateTTL(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		store := NewUpload(nil, nil)
		id := store.Mint()
		rec, done := startFeed(t.Context(), http.HandlerFunc(store.ServeProgress), id)
		time.Sleep(uploadIDTTL + time.Second)
		store.sweep(uploadIDTTL)
		synctest.Wait()
		if _, ok := store.get(id); ok || !ended(done) || !strings.HasSuffix(rec.text(), `"code":"invalid"}`+"\n") {
			t.Fatalf("idle watched receiver retained = %v, feed ended = %v with %q", ok, ended(done), rec.text())
		}
	})
}

// An evicted receiver's feed says so, and its still-valid id cannot quietly recreate an empty receiver.
func TestEvictedReceiverStaysGone(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		store := NewUpload(nil, nil)
		id := store.Mint()
		rec, done := startFeed(t.Context(), http.HandlerFunc(store.ServeProgress), id)
		fillStore(store)
		if _, ok := store.getOrCreate(store.Mint()); !ok {
			t.Fatal("the watched empty receiver was not displaced")
		}
		synctest.Wait()
		if !ended(done) || !strings.HasSuffix(rec.text(), `"code":"invalid"}`+"\n") {
			t.Fatalf("evicted feed ended = %v with %q", ended(done), rec.text())
		}
		store.sweep(uploadIDTTL)
		if _, access := store.accessFor(id, ownedBy("192.0.2.1"), true); access != uploadAccessInvalid {
			t.Fatalf("a lane on the evicted id = %v, want invalid", access)
		}
		time.Sleep(uploadTokenTTL + uploadSweepInterval)
		store.sweep(uploadIDTTL)
		if len(store.evicted) != 0 {
			t.Fatalf("%d evicted ids outlived their tokens", len(store.evicted))
		}
	})
}
