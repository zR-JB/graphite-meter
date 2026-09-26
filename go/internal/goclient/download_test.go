package goclient

import (
	"context"
	"errors"
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
	r := &runner{cfg: Config{BaseURL: srv.URL}.normalized(), streams: streamCounts{down: 1, up: 1}, http: srv.Client()}

	ctx, cancel := context.WithCancel(t.Context())
	defer cancel()
	var total atomic.Uint64
	done := make(chan struct{})
	go func() {
		_ = r.downloadLane(ctx, srv.URL, 0, &total, func() {})
		close(done)
	}()
	deadline := time.After(2 * time.Second)
	for total.Load() != size {
		select {
		case <-deadline:
			t.Fatalf("first download never completed: got %d bytes, want %d", total.Load(), size)
		case <-time.After(5 * time.Millisecond):
		}
	}
	cancel()
	select {
	case <-done:
	case <-time.After(2 * time.Second):
		t.Fatal("downloadLane did not return after context cancellation")
	}
	if got := total.Load(); got != size {
		t.Errorf("total after cancellation = %d, want %d (no partial second request counted)", got, size)
	}
}

func TestDownloadLaneReturnsAdmissionRejection(t *testing.T) {
	t.Parallel()
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		w.WriteHeader(http.StatusTooManyRequests)
	}))
	defer srv.Close()
	r := &runner{http: srv.Client()}
	var total atomic.Uint64
	if err := r.downloadLane(t.Context(), srv.URL, 0, &total, func() {}); err == nil {
		t.Fatal("HTTP 429 did not fail the download lane")
	}
}

func TestDownloadLaneReopensAfterAbruptConnectionDropAtAPace(t *testing.T) {
	t.Parallel()
	const partial = 64 * 1024
	const window = 1500 * time.Millisecond
	var requests atomic.Int64
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		requests.Add(1)
		w.WriteHeader(http.StatusOK)
		_, _ = w.Write(make([]byte, partial))
		w.(http.Flusher).Flush()
		panic(http.ErrAbortHandler)
	}))
	defer srv.Close()
	r := &runner{cfg: Config{BaseURL: srv.URL}.normalized(), streams: streamCounts{down: 1, up: 1}, http: srv.Client()}

	ctx, cancel := context.WithTimeout(t.Context(), window)
	defer cancel()
	var total atomic.Uint64
	done := make(chan struct{})
	go func() {
		_ = r.downloadLane(ctx, srv.URL, 0, &total, func() {})
		close(done)
	}()
	select {
	case <-done:
	case <-time.After(2 * time.Second):
		t.Fatal("downloadLane did not return after the stage ended")
	}
	if got := total.Load(); got < 2*partial {
		t.Errorf("total = %d, want at least %d (the lane must reopen after the drop)", got, 2*partial)
	}
	if paced := int64(window/wtRedialBackoff) + 2; requests.Load() > paced {
		t.Errorf("issued %d requests in %v, want at most %d: the reopen is not paced", requests.Load(), window, paced)
	}
}

func TestDownloadStageCancellation(t *testing.T) {
	t.Parallel()
	for _, c := range []struct {
		name        string
		silent      bool
		cancelAfter time.Duration
	}{
		{"streaming", false, 150 * time.Millisecond},
		{"silent server", true, 150 * time.Millisecond},
		{"already cancelled", false, -1},
	} {
		t.Run(c.name, func(t *testing.T) {
			t.Parallel()
			srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				w.WriteHeader(http.StatusOK)
				w.(http.Flusher).Flush()
				if c.silent {
					<-r.Context().Done()
					return
				}
				_, _ = w.Write(make([]byte, 64*1024))
			}))
			defer srv.Close()
			r := &runner{
				cfg:     Config{BaseURL: srv.URL}.normalized(),
				streams: streamCounts{down: 1, up: 1},
				http:    srv.Client(),
				emit:    func(Event) {},
			}
			ctx, cancel := context.WithCancel(t.Context())
			defer cancel()
			if c.cancelAfter < 0 {
				cancel()
			} else {
				time.AfterFunc(c.cancelAfter, cancel)
			}
			started := time.Now()
			result, err := r.testTransferResult(ctx, StageDownload, 5*time.Second)
			if !errors.Is(err, context.Canceled) || time.Since(started) > 1500*time.Millisecond {
				t.Fatalf("stage returned %v after %v, want a prompt context.Canceled", err, time.Since(started))
			}
			if c.silent && result.TotalBytes != 0 {
				t.Errorf("TotalBytes = %d, want 0 from a server that wrote nothing", result.TotalBytes)
			}
		})
	}
}
