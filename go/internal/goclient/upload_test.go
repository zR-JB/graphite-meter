package goclient

import (
	"bytes"
	"context"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"sync/atomic"
	"testing"
	"time"

	"github.com/coder/websocket"
	"github.com/zR-JB/graphite-meter/go/internal/wire"
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

func TestCyclingBodyStopsOnCancelledContext(t *testing.T) {
	ctx, cancel := context.WithCancel(context.Background())
	cancel()
	b := &cyclingBody{ctx: ctx, block: []byte{1, 2, 3}}
	if _, err := b.Read(make([]byte, 4)); err == nil {
		t.Fatal("want an error once the context is cancelled")
	}
}

func TestMintUploadID(t *testing.T) {
	newServer := func(handler http.HandlerFunc) *httptest.Server {
		return httptest.NewServer(handler)
	}

	t.Run("success", func(t *testing.T) {
		srv := newServer(func(w http.ResponseWriter, r *http.Request) {
			w.Header().Set("Content-Type", "application/json")
			_ = json.NewEncoder(w).Encode(uploadSessionResponse{UploadID: "abc-123"})
		})
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
		srv := newServer(func(w http.ResponseWriter, r *http.Request) {
			w.WriteHeader(http.StatusInternalServerError)
		})
		defer srv.Close()
		r := &runner{cfg: Config{BaseURL: srv.URL}, http: srv.Client()}
		if _, err := r.mintUploadID(context.Background()); err == nil {
			t.Fatal("want an error for a non-200 upload session response")
		}
	})

	t.Run("empty uploadId is an error", func(t *testing.T) {
		srv := newServer(func(w http.ResponseWriter, r *http.Request) {
			w.Header().Set("Content-Type", "application/json")
			_ = json.NewEncoder(w).Encode(uploadSessionResponse{})
		})
		defer srv.Close()
		r := &runner{cfg: Config{BaseURL: srv.URL}, http: srv.Client()}
		if _, err := r.mintUploadID(context.Background()); err == nil {
			t.Fatal("want an error for an empty uploadId")
		}
	})

	t.Run("malformed JSON is an error", func(t *testing.T) {
		srv := newServer(func(w http.ResponseWriter, r *http.Request) {
			w.Header().Set("Content-Type", "application/json")
			_, _ = w.Write([]byte("not json"))
		})
		defer srv.Close()
		r := &runner{cfg: Config{BaseURL: srv.URL}, http: srv.Client()}
		if _, err := r.mintUploadID(context.Background()); err == nil {
			t.Fatal("want an error for a malformed session response")
		}
	})
}

// TestUploadLaneDrainsBytes checks uploadLane streams its cycling body to the
// server and that the server-observed byte count grows without bound while
// the lane runs, then stops promptly on cancellation. The upload stream is
// intentionally endless (matching the real client, which relies on the caller
// to stop it once the stage's measurement window closes) so there is no
// natural "final total" to await; the test instead waits for a threshold to be
// crossed and confirms the lane joins quickly once cancelled.
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
		r.uploadLane(ctx, "test-id", 0, block)
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

// newFakeUploadServer wires up the three endpoints measureUpload depends on: a
// session mint, an upload sink that counts drained bytes, and a /ws/upload bus
// that reports that count back as server-authoritative BYTES_RECEIVED /
// UPLOAD_COMPLETE frames, mirroring go/internal/endpoint's real protocol.
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
	mux.HandleFunc("/ws/upload", func(w http.ResponseWriter, r *http.Request) {
		conn, err := websocket.Accept(w, r, &websocket.AcceptOptions{CompressionMode: websocket.CompressionDisabled})
		if err != nil {
			return
		}
		defer conn.Close(websocket.StatusNormalClosure, "")
		ctx := r.Context()

		bye := make(chan struct{})
		go func() {
			for {
				_, msg, err := conn.Read(ctx)
				if err != nil {
					close(bye)
					return
				}
				if f, derr := wire.Decode(string(msg)); derr == nil && f.Op == wire.OpBYE {
					close(bye)
					return
				}
			}
		}()

		ticker := time.NewTicker(20 * time.Millisecond)
		defer ticker.Stop()
		for {
			select {
			case <-ctx.Done():
				return
			case <-bye:
				n := served.Load()
				active := uint64(time.Since(started))
				_ = conn.Write(context.Background(), websocket.MessageText,
					[]byte(wire.Encode(wire.Frame{Op: wire.OpUploadComplete, N: n, Nanos: active})))
				return
			case <-ticker.C:
				n := served.Load()
				active := uint64(time.Since(started))
				_ = conn.Write(ctx, websocket.MessageText,
					[]byte(wire.Encode(wire.Frame{Op: wire.OpBytesReceived, N: n, Nanos: active})))
			}
		}
	})
	return httptest.NewServer(mux)
}

func TestMeasureUploadReportsServerAuthoritativeTotal(t *testing.T) {
	srv := newFakeUploadServer(t)
	defer srv.Close()

	cfg := Config{
		BaseURL:              srv.URL,
		ParallelStreams:      1,
		UploadProgressSettle: 20 * time.Millisecond,
	}.normalized()
	r := &runner{cfg: cfg, http: srv.Client(), emit: func(Event) {}}

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
