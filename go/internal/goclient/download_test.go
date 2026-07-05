package goclient

import (
	"context"
	"net/http"
	"net/http/httptest"
	"strconv"
	"sync/atomic"
	"testing"
	"time"
)

// newCountingDownloadServer serves the "bytes" query param's worth of data on
// the first request, then hangs on every later request until the client's
// context is cancelled — letting tests observe exactly one completed transfer
// before forcing cancellation.
func newCountingDownloadServer(size int) *httptest.Server {
	var reqs atomic.Int32
	return httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if reqs.Add(1) == 1 {
			_, _ = w.Write(make([]byte, size))
			return
		}
		<-r.Context().Done()
	}))
}

func TestDownloadLaneCountsExactBytes(t *testing.T) {
	const size = 256 * 1024
	srv := newCountingDownloadServer(size)
	defer srv.Close()

	cfg := Config{BaseURL: srv.URL, ParallelStreams: 1, DownloadBytesPerStream: size}.normalized()
	r := &runner{cfg: cfg, http: srv.Client()}

	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	var total atomic.Uint64
	done := make(chan struct{})
	go func() {
		r.downloadLane(ctx, srv.URL, 0, &total)
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

// newBytesEchoDownloadServer serves exactly the requested "bytes" query param
// on every request, matching the real /download endpoint's contract.
func newBytesEchoDownloadServer() *httptest.Server {
	return httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		n, err := strconv.ParseInt(r.URL.Query().Get("bytes"), 10, 64)
		if err != nil || n <= 0 {
			n = 64 * 1024
		}
		w.Header().Set("Content-Type", "application/octet-stream")
		_, _ = w.Write(make([]byte, n))
	}))
}

func TestMeasureDownloadReportsBytes(t *testing.T) {
	srv := newBytesEchoDownloadServer()
	defer srv.Close()

	cfg := Config{BaseURL: srv.URL, ParallelStreams: 1, DownloadBytesPerStream: 128 * 1024}.normalized()
	r := &runner{cfg: cfg, http: srv.Client(), emit: func(Event) {}}

	start := make(chan struct{})
	close(start)
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()

	res, err := r.measureDownload(ctx, "download", 350*time.Millisecond, start)
	if err != nil {
		t.Fatalf("measureDownload: %v", err)
	}
	if res.TotalBytes == 0 {
		t.Error("reported TotalBytes = 0, want > 0")
	}
	if res.Samples == 0 {
		t.Error("reported Samples = 0, want at least one throughput sample")
	}
	if res.MeanBps <= 0 {
		t.Errorf("MeanBps = %v, want > 0", res.MeanBps)
	}
}

// TestMeasureDownloadContextCancelStopsEarly checks that cancelling mid-measurement
// returns well before the configured elapsed window, and that the lane goroutines
// are joined (no hang) rather than left running.
func TestMeasureDownloadContextCancelStopsEarly(t *testing.T) {
	srv := newBytesEchoDownloadServer()
	defer srv.Close()

	cfg := Config{BaseURL: srv.URL, ParallelStreams: 1, DownloadBytesPerStream: 64 * 1024}.normalized()
	r := &runner{cfg: cfg, http: srv.Client(), emit: func(Event) {}}

	start := make(chan struct{})
	close(start)
	ctx, cancel := context.WithCancel(context.Background())
	time.AfterFunc(150*time.Millisecond, cancel)
	defer cancel()

	done := make(chan struct{})
	begin := time.Now()
	go func() {
		// elapsed is long (5s) so a hang would fail the test's own deadline
		// well before the stage's configured window would naturally end.
		_, _ = r.measureDownload(ctx, "download", 5*time.Second, start)
		close(done)
	}()

	select {
	case <-done:
	case <-time.After(2 * time.Second):
		t.Fatal("measureDownload did not stop after context cancellation")
	}
	if elapsed := time.Since(begin); elapsed > 1500*time.Millisecond {
		t.Errorf("measureDownload took %v to stop, want well under the 5s elapsed window", elapsed)
	}
}

// newAbruptCloseDownloadServer sends headers plus partial bytes over chunked
// encoding, then aborts the handler mid-response: the connection drops
// without a clean terminating chunk, unlike a normal completed transfer or a
// context-cancelled one.
func newAbruptCloseDownloadServer(partial int) *httptest.Server {
	return httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/octet-stream")
		w.WriteHeader(http.StatusOK)
		_, _ = w.Write(make([]byte, partial))
		w.(http.Flusher).Flush()
		panic(http.ErrAbortHandler)
	}))
}

// TestDownloadLaneStopsOnAbruptConnectionDrop checks that a server closing
// the connection mid-response (rather than a clean EOF) makes downloadLane
// return promptly with only the bytes actually delivered counted, instead of
// hanging or panicking on the broken stream.
func TestDownloadLaneStopsOnAbruptConnectionDrop(t *testing.T) {
	const partial = 64 * 1024
	srv := newAbruptCloseDownloadServer(partial)
	defer srv.Close()

	cfg := Config{BaseURL: srv.URL, ParallelStreams: 1, DownloadBytesPerStream: partial * 4}.normalized()
	r := &runner{cfg: cfg, http: srv.Client()}

	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
	defer cancel()
	var total atomic.Uint64
	done := make(chan struct{})
	go func() {
		r.downloadLane(ctx, srv.URL, 0, &total)
		close(done)
	}()

	select {
	case <-done:
	case <-time.After(1 * time.Second):
		t.Fatal("downloadLane hung after the server closed the connection abruptly mid-transfer")
	}
	if got := total.Load(); got == 0 || got > partial {
		t.Errorf("total = %d, want > 0 and <= %d (only the bytes sent before the abrupt drop)", got, partial)
	}
}

func TestMeasureDownloadReturnsImmediatelyWhenAlreadyCancelled(t *testing.T) {
	srv := newBytesEchoDownloadServer()
	defer srv.Close()

	cfg := Config{BaseURL: srv.URL, ParallelStreams: 1, DownloadBytesPerStream: 64 * 1024}.normalized()
	r := &runner{cfg: cfg, http: srv.Client(), emit: func(Event) {}}

	ctx, cancel := context.WithCancel(context.Background())
	cancel()
	start := make(chan struct{}) // never closed: still "warming up"

	_, err := r.measureDownload(ctx, "download", time.Second, start)
	if err == nil {
		t.Fatal("want an error when the context is already cancelled")
	}
}
