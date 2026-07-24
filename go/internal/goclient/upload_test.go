package goclient

import (
	"bytes"
	"context"
	"encoding/json"
	"io"
	"net/http"
	"net/http/httptest"
	"sync"
	"sync/atomic"
	"testing"
	"time"
)

func TestCyclingBodyWrapsDeterministically(t *testing.T) {
	block := []byte{1, 2, 3, 4, 5}
	b := &cyclingBody{ctx: context.Background(), block: block}

	buf := make([]byte, 12)
	n, err := b.Read(buf)
	if err != nil {
		t.Fatalf("Read: %v", err)
	}
	if n != len(buf) {
		t.Fatalf("Read returned n=%d, want %d", n, len(buf))
	}
	want := []byte{1, 2, 3, 4, 5, 1, 2, 3, 4, 5, 1, 2}
	if !bytes.Equal(buf, want) {
		t.Errorf("Read = %v, want %v", buf, want)
	}
}

func TestCyclingBodyStopsAtLimit(t *testing.T) {
	// limit exercises the known-Content-Length path: the body emits exactly
	// `limit` bytes (wrapping the block) and then reports io.EOF.
	b := &cyclingBody{ctx: context.Background(), block: []byte{1, 2, 3}, limit: 7}
	var got []byte
	buf := make([]byte, 4)
	for {
		n, err := b.Read(buf)
		got = append(got, buf[:n]...)
		if err == io.EOF {
			break
		}
		if err != nil {
			t.Fatalf("Read: %v", err)
		}
	}
	want := []byte{1, 2, 3, 1, 2, 3, 1}
	if !bytes.Equal(got, want) {
		t.Errorf("emitted %v, want %v (exactly limit bytes then EOF)", got, want)
	}
}

func TestCyclingBodyStopsOnCancelledContext(t *testing.T) {
	ctx, cancel := context.WithCancel(context.Background())
	cancel()
	b := &cyclingBody{ctx: ctx, block: []byte{1, 2, 3}}
	if _, err := b.Read(make([]byte, 4)); err == nil {
		t.Fatal("want an error once the context is cancelled")
	}
}

func TestMintUploadID(t *testing.T) {
	t.Run("success", func(t *testing.T) {
		srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
			w.Header().Set("Content-Type", "application/json")
			_ = json.NewEncoder(w).Encode(uploadSessionResponse{UploadID: "abc-123"})
		}))
		defer srv.Close()
		r := &runner{cfg: Config{BaseURL: srv.URL}, http: srv.Client()}
		id, err := r.mintUploadID(context.Background())
		if err != nil {
			t.Fatalf("mintUploadID: %v", err)
		}
		if id != "abc-123" {
			t.Errorf("id = %q, want abc-123", id)
		}
	})

	t.Run("non-200 response is an error", func(t *testing.T) {
		srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
			w.WriteHeader(http.StatusInternalServerError)
		}))
		defer srv.Close()
		r := &runner{cfg: Config{BaseURL: srv.URL}, http: srv.Client()}
		if _, err := r.mintUploadID(context.Background()); err == nil {
			t.Fatal("want an error for a non-200 upload session response")
		}
	})

	t.Run("empty uploadId is an error", func(t *testing.T) {
		srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
			w.Header().Set("Content-Type", "application/json")
			_ = json.NewEncoder(w).Encode(uploadSessionResponse{})
		}))
		defer srv.Close()
		r := &runner{cfg: Config{BaseURL: srv.URL}, http: srv.Client()}
		if _, err := r.mintUploadID(context.Background()); err == nil {
			t.Fatal("want an error for an empty uploadId")
		}
	})

	t.Run("malformed JSON is an error", func(t *testing.T) {
		srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
			w.Header().Set("Content-Type", "application/json")
			_, _ = w.Write([]byte("not json"))
		}))
		defer srv.Close()
		r := &runner{cfg: Config{BaseURL: srv.URL}, http: srv.Client()}
		if _, err := r.mintUploadID(context.Background()); err == nil {
			t.Fatal("want an error for a malformed session response")
		}
	})
}

// TestUploadLaneDrainsBytes checks uploadLane streams its cycling body until
// cancelled. The stream is endless by design, since the caller stops it when
// the stage's window closes, so the test waits for a byte threshold instead of
// a final total, then confirms the lane joins promptly.
func TestUploadLaneDrainsBytes(t *testing.T) {
	var served atomic.Uint64
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		buf := make([]byte, 32*1024)
		for {
			n, err := r.Body.Read(buf)
			if n > 0 {
				served.Add(uint64(n))
			}
			if err != nil {
				return
			}
		}
	}))
	defer srv.Close()

	r := &runner{cfg: Config{BaseURL: srv.URL}, http: srv.Client()}
	block := make([]byte, 64*1024)

	ctx, cancel := context.WithCancel(context.Background())
	done := make(chan struct{})
	go func() {
		_ = r.uploadLane(ctx, "test-id", 0, block)
		close(done)
	}()

	const threshold = 3 * 64 * 1024 // several multiples of the block size
	deadline := time.After(2 * time.Second)
	for served.Load() < threshold {
		select {
		case <-deadline:
			cancel()
			t.Fatalf("server only observed %d bytes, want at least %d", served.Load(), threshold)
		case <-time.After(5 * time.Millisecond):
		}
	}

	cancel()
	select {
	case <-done:
	case <-time.After(2 * time.Second):
		t.Fatal("uploadLane did not return after context cancellation")
	}
}

func TestUploadLaneReturnsAdmissionRejection(t *testing.T) {
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		w.WriteHeader(http.StatusServiceUnavailable)
	}))
	defer srv.Close()
	r := &runner{cfg: Config{BaseURL: srv.URL, UploadBytesPerStream: 1024}, http: srv.Client()}
	if err := r.uploadLane(context.Background(), "test-id", 0, make([]byte, 1024)); err == nil {
		t.Fatal("HTTP 503 did not fail the upload lane")
	}
}

// newAbruptCloseUploadServer reads a little of each request's body then aborts
// the handler, dropping the connection without a response. It simulates a
// server that vanishes mid-transfer rather than one that responds cleanly or
// is merely slow.
func newAbruptCloseUploadServer() *httptest.Server {
	return httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		buf := make([]byte, 8*1024)
		_, _ = r.Body.Read(buf)
		panic(http.ErrAbortHandler)
	}))
}

// TestUploadLaneSurvivesAbruptConnectionDrop checks that a lane whose every
// request is abruptly dropped mid-transfer keeps retrying without panicking
// or hanging, and still joins promptly once cancelled.
func TestUploadLaneSurvivesAbruptConnectionDrop(t *testing.T) {
	srv := newAbruptCloseUploadServer()
	defer srv.Close()

	r := &runner{cfg: Config{BaseURL: srv.URL}, http: srv.Client()}
	block := make([]byte, 64*1024)

	ctx, cancel := context.WithCancel(context.Background())
	done := make(chan struct{})
	go func() {
		_ = r.uploadLane(ctx, "test-id", 0, block)
		close(done)
	}()

	// The sleep lets the lane hit and retry past several abrupt drops.
	time.Sleep(150 * time.Millisecond)
	cancel()
	select {
	case <-done:
	case <-time.After(2 * time.Second):
		t.Fatal("uploadLane did not return after repeated abrupt connection drops plus cancellation")
	}
}

func mountFakeProgress(mux *http.ServeMux, served *atomic.Uint64, started time.Time) {
	finished := make(chan struct{})
	var once sync.Once
	mux.HandleFunc("/upload/progress", func(w http.ResponseWriter, r *http.Request) {
		if r.Method == http.MethodDelete {
			once.Do(func() { close(finished) })
			w.WriteHeader(http.StatusNoContent)
			return
		}
		w.Header().Set("Content-Type", "application/x-ndjson")
		flusher := w.(http.Flusher)
		_ = json.NewEncoder(w).Encode(uploadProgressEvent{Type: "ready"})
		flusher.Flush()
		ticker := time.NewTicker(20 * time.Millisecond)
		defer ticker.Stop()
		for {
			select {
			case <-r.Context().Done():
				return
			case <-finished:
				_ = json.NewEncoder(w).Encode(uploadProgressEvent{Type: "complete", Bytes: served.Load(), Nanos: uint64(time.Since(started))})
				flusher.Flush()
				return
			case <-ticker.C:
				_ = json.NewEncoder(w).Encode(uploadProgressEvent{Type: "progress", Bytes: served.Load(), Nanos: uint64(time.Since(started))})
				flusher.Flush()
			}
		}
	})
}

// newFakeUploadServer wires the session, upload sink, and throughput-bound
// NDJSON progress stream used by measureUpload.
func newFakeUploadServer(t *testing.T) *httptest.Server {
	t.Helper()
	var served atomic.Uint64
	started := time.Now()

	mux := http.NewServeMux()
	mux.HandleFunc("/upload/session", func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		_ = json.NewEncoder(w).Encode(uploadSessionResponse{UploadID: "test-upload"})
	})
	mux.HandleFunc("/upload", func(w http.ResponseWriter, r *http.Request) {
		buf := make([]byte, 32*1024)
		for {
			n, err := r.Body.Read(buf)
			if n > 0 {
				served.Add(uint64(n))
			}
			if err != nil {
				return
			}
		}
	})
	mountFakeProgress(mux, &served, started)
	return httptest.NewServer(mux)
}

func TestMeasureUploadReportsServerAuthoritativeTotal(t *testing.T) {
	srv := newFakeUploadServer(t)
	defer srv.Close()

	cfg := Config{
		BaseURL:         srv.URL,
		TransferStreams: TransferStreamPolicy{Forced: 1},
	}.normalized()
	r := &runner{cfg: cfg, streams: 1, http: srv.Client(), emit: func(Event) {}}
	attachTestLatencyTarget(r, srv.URL)

	start := make(chan struct{})
	close(start)
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()

	res, err := r.measureUpload(ctx, "upload", 300*time.Millisecond, start)
	if err != nil {
		t.Fatalf("measureUpload: %v", err)
	}
	if !res.ServerAuth {
		t.Error("upload Result.ServerAuth = false, want true")
	}
	if res.TotalBytes == 0 {
		t.Error("reported TotalBytes = 0, want > 0")
	}
}

func TestUploadProgressWaitNext(t *testing.T) {
	t.Run("already advanced", func(t *testing.T) {
		progress := &uploadProgress{done: make(chan struct{}), changed: make(chan struct{}, 1)}
		progress.seq.Store(2)
		if !progress.waitNext(context.Background(), 1) {
			t.Fatal("waitNext rejected an available update")
		}
	})

	t.Run("notification", func(t *testing.T) {
		progress := &uploadProgress{done: make(chan struct{}), changed: make(chan struct{}, 1)}
		result := make(chan bool, 1)
		go func() { result <- progress.waitNext(context.Background(), 0) }()
		progress.seq.Store(1)
		progress.changed <- struct{}{}
		if !<-result {
			t.Fatal("waitNext ignored a progress edge")
		}
	})

	t.Run("cancellation", func(t *testing.T) {
		progress := &uploadProgress{done: make(chan struct{}), changed: make(chan struct{}, 1)}
		ctx, cancel := context.WithCancel(context.Background())
		cancel()
		if progress.waitNext(ctx, 0) {
			t.Fatal("waitNext succeeded after cancellation")
		}
	})

	t.Run("terminal", func(t *testing.T) {
		done := make(chan struct{})
		close(done)
		progress := &uploadProgress{done: done, changed: make(chan struct{}, 1)}
		if progress.waitNext(context.Background(), 0) {
			t.Fatal("waitNext succeeded after the progress stream closed")
		}
	})

	t.Run("final update", func(t *testing.T) {
		done := make(chan struct{})
		close(done)
		progress := &uploadProgress{done: done, changed: make(chan struct{}, 1)}
		progress.seq.Store(1)
		if !progress.waitNext(context.Background(), 0) {
			t.Fatal("waitNext dropped the final progress update")
		}
	})
}
