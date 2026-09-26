package goclient

import (
	"bytes"
	"context"
	"errors"
	"io"
	"net/http"
	"net/http/httptest"
	"sync/atomic"
	"testing"
	"time"
)

func TestDownloadLaneCountsExactBytes(t *testing.T) {
	t.Parallel()
	const size = 256 * 1024
	var requests atomic.Int32
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if requests.Add(1) == 1 {
			_, _ = w.Write(make([]byte, size))
			return
		}
		<-r.Context().Done()
	}))
	defer srv.Close()
	ctx, cancel := context.WithCancel(t.Context())
	var total atomic.Uint64
	done := make(chan struct{})
	go func() {
		_ = testRunner(srv).downloadLane(ctx, srv.URL, 0, &total, func() {})
		close(done)
	}()
	for requests.Load() < 2 {
		time.Sleep(5 * time.Millisecond)
	}
	cancel()
	<-done
	if got := total.Load(); got != size {
		t.Errorf("total = %d, want %d with no partial second request counted", got, size)
	}
}

func TestCyclingBody(t *testing.T) {
	t.Parallel()
	b := &cyclingBody{ctx: t.Context(), block: []byte{1, 2, 3}, remaining: 7}
	got, err := io.ReadAll(b)
	if want := []byte{1, 2, 3, 1, 2, 3, 1}; err != nil || !bytes.Equal(got, want) {
		t.Errorf("emitted %v, %v; want %v", got, err, want)
	}
	ctx, cancel := context.WithCancel(t.Context())
	cancel()
	if _, err := (&cyclingBody{ctx: ctx, block: []byte{1}, remaining: 1}).Read(make([]byte, 4)); err == nil {
		t.Fatal("a cancelled request kept reading")
	}
}

func TestMintUploadID(t *testing.T) {
	t.Parallel()
	for body, want := range map[string]string{
		`{"uploadId":"abc-123"}`: "abc-123",
		`{}`:                     "",
		`not json`:               "",
		"":                       "",
	} {
		srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
			if body == "" {
				w.WriteHeader(http.StatusInternalServerError)
			}
			_, _ = io.WriteString(w, body)
		}))
		id, err := testRunner(srv).mintUploadID(t.Context())
		srv.Close()
		if id != want || (err == nil) != (want != "") {
			t.Errorf("session response %q minted %q, %v; want %q", body, id, err, want)
		}
	}
}

func TestUploadSessionIsReleasedWhenSetupFails(t *testing.T) {
	t.Parallel()
	released := make(chan string, 1)
	mux := http.NewServeMux()
	mux.HandleFunc("/upload/session", func(w http.ResponseWriter, _ *http.Request) {
		_, _ = io.WriteString(w, `{"uploadId":"minted"}`)
	})
	mux.HandleFunc("/upload/progress", func(w http.ResponseWriter, r *http.Request) {
		if r.Method == http.MethodDelete {
			released <- r.URL.Query().Get("id")
		}
		w.WriteHeader(http.StatusServiceUnavailable)
	})
	srv := httptest.NewServer(mux)
	defer srv.Close()
	r := testRunner(srv)
	r.teardown = t.Context()
	if err := r.measureUpload(t.Context(), testStageGate(make(chan struct{}))); err == nil {
		t.Fatal("upload measured without a progress feed")
	}
	select {
	case id := <-released:
		if id != "minted" {
			t.Fatalf("released upload %q, want the minted session", id)
		}
	default:
		t.Fatal("a failed setup left the minted upload session open")
	}
}

func lane(r *runner, dir Direction, base string) func(context.Context) error {
	if dir == Down {
		var total atomic.Uint64
		return func(ctx context.Context) error { return r.downloadLane(ctx, base, 0, &total, func() {}) }
	}
	return func(ctx context.Context) error { return r.uploadLane(ctx, "id", 0, make([]byte, 64*1024), func() {}) }
}

func TestLanesFailOnAdmissionRejection(t *testing.T) {
	t.Parallel()
	for _, dir := range []Direction{Down, Up} {
		srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
			w.WriteHeader(http.StatusServiceUnavailable)
		}))
		err := lane(testRunner(srv), dir, srv.URL)(t.Context())
		srv.Close()
		if err == nil {
			t.Errorf("%s lane ignored HTTP 503", dir)
		}
	}
}

func TestLanesReopenDroppedConnectionsAtAPace(t *testing.T) {
	t.Parallel()
	const window = 1500 * time.Millisecond
	for _, dir := range []Direction{Down, Up} {
		t.Run(string(dir), func(t *testing.T) {
			t.Parallel()
			var requests atomic.Int64
			srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				requests.Add(1)
				if r.Method == http.MethodGet {
					_, _ = w.Write(make([]byte, 64*1024))
					w.(http.Flusher).Flush()
				}
				_, _ = r.Body.Read(make([]byte, 8*1024))
				panic(http.ErrAbortHandler)
			}))
			defer srv.Close()
			ctx, cancel := context.WithTimeout(t.Context(), window)
			defer cancel()
			if err := lane(testRunner(srv), dir, srv.URL)(ctx); err != nil {
				t.Fatalf("dropped connections failed the lane: %v", err)
			}
			if n := requests.Load(); n < 2 || n > int64(window/retryBackoff)+2 {
				t.Errorf("%d requests in %v, want reopened at most every %v", n, window, retryBackoff)
			}
		})
	}
}

func TestStageCancellationIsPrompt(t *testing.T) {
	t.Parallel()
	for _, c := range []struct {
		name        string
		stage       Stage
		mount       func(*http.ServeMux)
		cancelAfter time.Duration
	}{
		{"streaming download", StageDownload, func(mux *http.ServeMux) { mux.HandleFunc("/download", writeDownload) },
			150 * time.Millisecond},
		{"silent download", StageDownload, func(mux *http.ServeMux) {
			mux.HandleFunc("/download", func(w http.ResponseWriter, r *http.Request) {
				w.(http.Flusher).Flush()
				<-r.Context().Done()
			})
		}, 150 * time.Millisecond},
		{"already cancelled", StageDownload, func(mux *http.ServeMux) { mux.HandleFunc("/download", writeDownload) },
			0},
		{"stalled upload", StageUpload, func(mux *http.ServeMux) {
			mountUploadReceiver(mux, new(atomic.Uint64), discardUpload)
		}, 200 * time.Millisecond},
	} {
		t.Run(c.name, func(t *testing.T) {
			t.Parallel()
			mux := http.NewServeMux()
			c.mount(mux)
			srv := httptest.NewServer(mux)
			defer srv.Close()
			r := testRunner(srv)
			var outcome Outcome
			r.emit = func(e Event) {
				if e.Kind == EventDone {
					outcome = e.Outcome()
				}
			}
			ctx, cancel := context.WithCancel(t.Context())
			defer cancel()
			time.AfterFunc(c.cancelAfter, cancel)
			started := time.Now()
			result, err := r.testTransferResult(ctx, c.stage, 5*time.Second)
			elapsed := time.Since(started)
			if !errors.Is(err, context.Canceled) || elapsed > 1500*time.Millisecond || outcome != OutcomeStopped {
				t.Fatalf("stage returned %v (%s) after %v, want a prompt stop", err, outcome, elapsed)
			}
			if c.name == "silent download" && result.TotalBytes != 0 {
				t.Errorf("TotalBytes = %d from a server that wrote nothing", result.TotalBytes)
			}
		})
	}
}
