package endpoint

import (
	"bytes"
	"context"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync"
	"testing"
	"time"
)

// progressRecorder is a flushable, race-safe ResponseWriter: the handler under
// test streams from its own goroutine while the test reads the body, and wrote
// wakes the reader instead of making it poll.
type progressRecorder struct {
	mu     sync.Mutex
	header http.Header
	body   bytes.Buffer
	wrote  chan struct{}
}

func newProgressRecorder() *progressRecorder {
	return &progressRecorder{header: make(http.Header), wrote: make(chan struct{}, 1)}
}

func (r *progressRecorder) Header() http.Header { return r.header }
func (r *progressRecorder) WriteHeader(int)     {}
func (r *progressRecorder) Write(p []byte) (int, error) {
	r.mu.Lock()
	n, err := r.body.Write(p)
	r.mu.Unlock()
	r.notify()
	return n, err
}
func (r *progressRecorder) Flush() { r.notify() }

// notify is a non-blocking nudge: waitProgressText re-reads the body after every
// wake, so a coalesced signal loses nothing.
func (r *progressRecorder) notify() {
	select {
	case r.wrote <- struct{}{}:
	default:
	}
}

func (r *progressRecorder) text() string {
	r.mu.Lock()
	defer r.mu.Unlock()
	return r.body.String()
}

func waitProgressText(t *testing.T, r *progressRecorder, part string) {
	t.Helper()
	deadline := time.NewTimer(3 * time.Second)
	defer deadline.Stop()
	for !strings.Contains(r.text(), part) {
		select {
		case <-r.wrote:
		case <-deadline.C:
			t.Fatalf("progress stream never contained %q: %s", part, r.text())
		}
	}
}

func TestUploadProgressNDJSONLifecycle(t *testing.T) {
	store := NewUploadStore()
	id := store.Mint()
	h := httpAdapter(NewUploadProgress(store))
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	req := httptest.NewRequest(http.MethodGet, "/upload/progress?id="+id, nil).WithContext(ctx)
	rec := newProgressRecorder()
	done := make(chan struct{})
	go func() { h.ServeHTTP(rec, req); close(done) }()

	waitProgressText(t, rec, `{"type":"ready"}`)
	if got, want := rec.Header().Get("Content-Type"), "application/x-ndjson"; got != want {
		t.Fatalf("Content-Type = %q, want %q", got, want)
	}
	if got, want := rec.Header().Get("Cache-Control"), "no-store, no-transform"; got != want {
		t.Fatalf("Cache-Control = %q, want %q", got, want)
	}
	agg, ok := store.get(id)
	if !ok {
		t.Fatal("aggregate not created by progress GET")
	}
	agg.changePosts(1)
	agg.recordChunk(monoNanos(), 4096)
	waitProgressText(t, rec, `"type":"progress","bytes":4096`)
	agg.changePosts(-1)

	finish := httptest.NewRecorder()
	h.ServeHTTP(finish, httptest.NewRequest(http.MethodDelete, "/upload/progress?id="+id, nil))
	if finish.Code != http.StatusNoContent {
		t.Fatalf("DELETE status = %d, want %d", finish.Code, http.StatusNoContent)
	}
	waitProgressText(t, rec, `"type":"complete","bytes":4096`)
	select {
	case <-done:
	case <-time.After(time.Second):
		t.Fatal("progress GET did not terminate after explicit finalization")
	}

	// Completion is replayable until the aggregate TTL expires, so a dropped
	// terminal response cannot strand a reconnecting client.
	replay := httptest.NewRecorder()
	h.ServeHTTP(replay, httptest.NewRequest(http.MethodGet, "/upload/progress?id="+id, nil))
	if want := `"type":"complete","bytes":4096`; !strings.Contains(replay.Body.String(), want) {
		t.Fatalf("replayed body = %s, want it to contain %q", replay.Body.String(), want)
	}
}

func TestUploadProgressRejectsUnknownID(t *testing.T) {
	rec := httptest.NewRecorder()
	httpAdapter(NewUploadProgress(NewUploadStore())).ServeHTTP(rec, httptest.NewRequest(http.MethodGet, "/upload/progress?id=forged", nil))
	if rec.Code != http.StatusBadRequest {
		t.Fatalf("status = %d, want %d", rec.Code, http.StatusBadRequest)
	}
	if !strings.Contains(rec.Body.String(), "unknown upload id") {
		t.Fatalf("body = %s, want it to contain %q", rec.Body.String(), "unknown upload id")
	}
}

func TestUploadProgressRejectsDuplicateStream(t *testing.T) {
	store := NewUploadStore()
	id := store.Mint()
	h := httpAdapter(NewUploadProgress(store))
	ctx, cancel := context.WithCancel(context.Background())
	rec := newProgressRecorder()
	done := make(chan struct{})
	go func() {
		h.ServeHTTP(rec, httptest.NewRequest(http.MethodGet, "/upload/progress?id="+id, nil).WithContext(ctx))
		close(done)
	}()
	waitProgressText(t, rec, `{"type":"ready"}`)

	duplicate := httptest.NewRecorder()
	h.ServeHTTP(duplicate, httptest.NewRequest(http.MethodGet, "/upload/progress?id="+id, nil))
	if duplicate.Code != http.StatusConflict {
		t.Fatalf("duplicate status = %d, want %d", duplicate.Code, http.StatusConflict)
	}
	cancel()
	<-done
}

func TestUploadProgressDoesNotRefreshAggregateTTL(t *testing.T) {
	store := NewUploadStore()
	id := store.Mint()
	h := httpAdapter(NewUploadProgress(store))
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	rec := newProgressRecorder()
	done := make(chan struct{})
	go func() {
		h.ServeHTTP(rec, httptest.NewRequest(http.MethodGet, "/upload/progress?id="+id, nil).WithContext(ctx))
		close(done)
	}()
	waitProgressText(t, rec, `{"type":"ready"}`)
	agg, _ := store.get(id)
	old := monoNanos() - int64(2*uploadIDTTL)
	agg.lastTouchMono.Store(old)
	time.Sleep(2 * uploadProgressTick)
	if got := agg.lastTouchMono.Load(); got != old {
		t.Fatalf("progress tick refreshed last touch: got %d want %d", got, old)
	}
	store.sweep(uploadIDTTL)
	select {
	case <-done:
	case <-time.After(time.Second):
		t.Fatal("reaped aggregate did not close its progress stream")
	}
	if _, ok := store.get(id); ok {
		t.Fatal("idle aggregate survived without upload activity")
	}
}
