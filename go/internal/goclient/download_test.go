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
// context is cancelled. A test sees exactly one completed transfer, then
// forces cancellation.
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

	cfg := Config{BaseURL: srv.URL, TransferStreams: TransferStreamPolicy{Forced: 1}, DownloadBytesPerStream: size}.normalized()
	r := &runner{cfg: cfg, streams: streamCounts{down: 1, up: 1}, http: srv.Client()}

	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	var total atomic.Uint64
	done := make(chan struct{})
	go func() {
		_ = r.downloadLane(ctx, srv.URL, 0, &total)
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

	cfg := Config{BaseURL: srv.URL, TransferStreams: TransferStreamPolicy{Forced: 1}, DownloadBytesPerStream: 128 * 1024}.normalized()
	r := &runner{cfg: cfg, streams: streamCounts{down: 1, up: 1}, http: srv.Client(), emit: func(Event) {}}

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

func TestDownloadLaneReturnsAdmissionRejection(t *testing.T) {
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		w.WriteHeader(http.StatusTooManyRequests)
	}))
	defer srv.Close()
	r := &runner{cfg: Config{DownloadBytesPerStream: 1024}, http: srv.Client()}
	var total atomic.Uint64
	if err := r.downloadLane(context.Background(), srv.URL, 0, &total); err == nil {
		t.Fatal("HTTP 429 did not fail the download lane")
	}
}

// TestMeasureDownloadContextCancelStopsEarly checks that cancelling
// mid-measurement returns well under the configured elapsed window, and that
// the lane goroutines are joined (no hang) rather than left running.
func TestMeasureDownloadContextCancelStopsEarly(t *testing.T) {
	srv := newBytesEchoDownloadServer()
	defer srv.Close()

	cfg := Config{BaseURL: srv.URL, TransferStreams: TransferStreamPolicy{Forced: 1}, DownloadBytesPerStream: 64 * 1024}.normalized()
	r := &runner{cfg: cfg, streams: streamCounts{down: 1, up: 1}, http: srv.Client(), emit: func(Event) {}}

	start := make(chan struct{})
	close(start)
	ctx, cancel := context.WithCancel(context.Background())
	time.AfterFunc(150*time.Millisecond, cancel)
	defer cancel()

	done := make(chan struct{})
	begin := time.Now()
	go func() {
		// The window is long (5s), so a hang trips the test's own deadline well
		// under the stage's configured window.
		_, _ = r.measureDownload(ctx, "download", 5*time.Second, start)
		close(done)
	}()

	select {
	case <-done:
	case <-time.After(2 * time.Second):
		t.Fatal("measureDownload did not stop after context cancellation")
	}
	if elapsed := time.Since(begin); elapsed > 1500*time.Millisecond {
		t.Errorf("measureDownload took %v to stop, want well under the 5s measurement window", elapsed)
	}
}

// newAbruptCloseDownloadServer sends headers plus partial bytes over chunked
// encoding, then aborts the handler mid-response: the connection drops
// without a clean terminating chunk, unlike a normal completed transfer or a
// context-cancelled one. requests counts how often a lane reopened.
func newAbruptCloseDownloadServer(partial int, requests *atomic.Int64) *httptest.Server {
	return httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if requests != nil {
			requests.Add(1)
		}
		w.Header().Set("Content-Type", "application/octet-stream")
		w.WriteHeader(http.StatusOK)
		_, _ = w.Write(make([]byte, partial))
		w.(http.Flusher).Flush()
		panic(http.ErrAbortHandler)
	}))
}

// TestDownloadLaneReopensAfterAbruptConnectionDropAtAPace checks that a server
// closing the connection mid-response (rather than a clean EOF) makes
// downloadLane reopen and keep counting delivered bytes, so a stage outliving
// the server's request bound continues — but at a retry cadence, since a
// hard-down server would otherwise be hot-retried by every lane at once. The
// lane must still return promptly once the stage ends.
func TestDownloadLaneReopensAfterAbruptConnectionDropAtAPace(t *testing.T) {
	const partial = 64 * 1024
	const window = 1500 * time.Millisecond
	var requests atomic.Int64
	srv := newAbruptCloseDownloadServer(partial, &requests)
	defer srv.Close()

	cfg := Config{BaseURL: srv.URL, TransferStreams: TransferStreamPolicy{Forced: 1}, DownloadBytesPerStream: partial * 4}.normalized()
	r := &runner{cfg: cfg, streams: streamCounts{down: 1, up: 1}, http: srv.Client()}

	ctx, cancel := context.WithTimeout(context.Background(), window)
	defer cancel()
	var total atomic.Uint64
	done := make(chan struct{})
	start := time.Now()
	go func() {
		_ = r.downloadLane(ctx, srv.URL, 0, &total)
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
	// One request per backoff interval, plus the first: anything near the
	// thousands an unpaced loop would issue is the regression.
	if paced := int64(window/wtRedialBackoff) + 2; requests.Load() > paced {
		t.Errorf("issued %d requests in %v, want at most %d: the reopen is not paced",
			requests.Load(), time.Since(start), paced)
	}
}

func TestMeasureDownloadReturnsImmediatelyWhenAlreadyCancelled(t *testing.T) {
	srv := newBytesEchoDownloadServer()
	defer srv.Close()

	cfg := Config{BaseURL: srv.URL, TransferStreams: TransferStreamPolicy{Forced: 1}, DownloadBytesPerStream: 64 * 1024}.normalized()
	r := &runner{cfg: cfg, streams: streamCounts{down: 1, up: 1}, http: srv.Client(), emit: func(Event) {}}

	ctx, cancel := context.WithCancel(context.Background())
	cancel()
	start := make(chan struct{}) // never closed: still "warming up"

	_, err := r.measureDownload(ctx, "download", time.Second, start)
	if err == nil {
		t.Fatal("want an error when the context is already cancelled")
	}
}
