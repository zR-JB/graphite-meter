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

type progressRecorder struct {
	mu sync.Mutex
	h  http.Header
	b  bytes.Buffer
	n  chan struct{}
}

func newProgressRecorder() *progressRecorder {
	return &progressRecorder{h: make(http.Header), n: make(chan struct{}, 1)}
}
func (r *progressRecorder) Header() http.Header { return r.h }
func (r *progressRecorder) WriteHeader(int)     {}
func (r *progressRecorder) Write(p []byte) (int, error) {
	r.mu.Lock()
	n, err := r.b.Write(p)
	r.mu.Unlock()
	select {
	case r.n <- struct{}{}:
	default:
	}
	return n, err
}
func (r *progressRecorder) Flush() {
	select {
	case r.n <- struct{}{}:
	default:
	}
}
func (r *progressRecorder) text() string {
	r.mu.Lock()
	defer r.mu.Unlock()
	return r.b.String()
}

func waitProgressText(t *testing.T, r *progressRecorder, part string) {
	t.Helper()
	deadline := time.NewTimer(3 * time.Second)
	defer deadline.Stop()
	for !strings.Contains(r.text(), part) {
		select {
		case <-r.n:
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
	if got := rec.Header().Get("Content-Type"); got != "application/x-ndjson" {
		t.Fatalf("Content-Type = %q", got)
	}
	if got := rec.Header().Get("Cache-Control"); got != "no-store, no-transform" {
		t.Fatalf("Cache-Control = %q", got)
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
		t.Fatalf("DELETE status = %d", finish.Code)
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
	if !strings.Contains(replay.Body.String(), `"type":"complete","bytes":4096`) {
		t.Fatalf("replayed body = %s", replay.Body.String())
	}
}

func TestUploadProgressRejectsUnknownID(t *testing.T) {
	rec := httptest.NewRecorder()
	httpAdapter(NewUploadProgress(NewUploadStore())).ServeHTTP(rec, httptest.NewRequest(http.MethodGet, "/upload/progress?id=forged", nil))
	if rec.Code != http.StatusBadRequest {
		t.Fatalf("status = %d", rec.Code)
	}
	if !strings.Contains(rec.Body.String(), "unknown upload id") {
		t.Fatalf("body = %s", rec.Body.String())
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
		t.Fatalf("duplicate status = %d", duplicate.Code)
	}
	cancel()
	<-done
}

func TestUploadProgressDoesNotRefreshAggregateTTL(t *testing.T) {
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
	agg, _ := store.get(id)
	old := monoNanos() - int64(2*uploadIDTTL)
	agg.lastTouchMono.Store(old)
	time.Sleep(2 * uploadProgressTick)
	if got := agg.lastTouchMono.Load(); got != old {
		t.Fatalf("progress tick refreshed last touch: got %d want %d", got, old)
	}
	cancel()
	<-done
	store.sweep(uploadIDTTL)
	if _, ok := store.get(id); ok {
		t.Fatal("idle aggregate survived without upload activity")
	}
}
