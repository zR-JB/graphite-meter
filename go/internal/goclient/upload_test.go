package goclient

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"net/http/httptest"
	"runtime"
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
	r := &runner{cfg: cfg, streams: streamCounts{down: 1, up: 1}, http: srv.Client(), emit: func(Event) {}}
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

// TestUploadProgressHoldsTheForwardPairAcrossFeeds covers the interleaving two
// readers of one aggregate produce: the live feed publishes the terminal
// `complete` record, then the superseded feed lands a record it had already
// buffered. The server repeats the byte total on `complete`, so the two carry
// equal bytes and only the active time separates them.
func TestUploadProgressHoldsTheForwardPairAcrossFeeds(t *testing.T) {
	live, liveWriter := io.Pipe()
	go func() {
		defer liveWriter.Close()
		enc := json.NewEncoder(liveWriter)
		_ = enc.Encode(uploadProgressEvent{Type: "ready"})
		_ = enc.Encode(uploadProgressEvent{Type: "complete", Bytes: 1000, Nanos: uint64(5 * time.Second)})
	}()

	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	r := &runner{cfg: DefaultConfig(), emit: func(Event) {}}
	p, err := r.readUploadProgress(ctx, live, "http://127.0.0.1/upload/progress")
	if err != nil {
		t.Fatalf("readUploadProgress: %v", err)
	}
	defer p.close()
	if !p.waitNext(ctx, 0) {
		t.Fatal("the live feed never published a count")
	}

	stale, staleWriter := io.Pipe()
	p.attach(stale)
	if err := json.NewEncoder(staleWriter).Encode(uploadProgressEvent{Type: "progress", Bytes: 1000, Nanos: uint64(3200 * time.Millisecond)}); err != nil {
		t.Fatalf("write the superseded feed's buffered record: %v", err)
	}
	staleWriter.Close()
	<-p.currentDone()

	bytes, nanos := p.counters()
	if bytes != 1000 || nanos != uint64(5*time.Second) {
		t.Fatalf("counters = (%d bytes, %v), want (1000 bytes, 5s): the superseded feed walked the pair backwards", bytes, time.Duration(nanos))
	}
}

// TestSampleServerUploadWindowHoldsTheHighestPair checks the final window is
// priced off the highest pair the loop saw rather than whatever counters()
// happens to hold. A shorter active time over the same bytes inflates the rate,
// and one below the baseline drops the window entirely. The stale pair is
// stored past advance() so the window is tested on its own.
func TestSampleServerUploadWindowHoldsTheHighestPair(t *testing.T) {
	const baselineN = uint64(1000)
	const baselineT = uint64(time.Second)

	p := &uploadProgress{changed: make(chan struct{}, 1)}
	p.advance(3000, uint64(3*time.Second))

	sampled := make(chan struct{}, 1)
	r := &runner{cfg: DefaultConfig(), emit: func(Event) {
		select {
		case sampled <- struct{}{}:
		default:
		}
	}}

	laneErr := make(chan error, 1)
	type outcome struct {
		stats rateStats
		err   error
	}
	done := make(chan outcome, 1)
	go func() {
		stats, err := r.sampleServerUpload(context.Background(), "upload", p, 1, 5*time.Second, baselineN, baselineT, laneErr)
		done <- outcome{stats, err}
	}()

	select {
	case <-sampled:
	case <-time.After(5 * time.Second):
		t.Fatal("the sampler never folded in the server's counters")
	}
	p.count.Store(&uploadCount{bytes: 3000, nanos: uint64(1500 * time.Millisecond)})
	laneErr <- fmt.Errorf("lane ended")

	got := <-done
	if got.stats.total != 2000 || got.stats.elapsed != 2*time.Second {
		t.Fatalf("window = %d bytes over %v, want 2000 bytes over 2s", got.stats.total, got.stats.elapsed)
	}
}

// newWaitNextProgress builds a progress channel whose own context decides when
// the report is over, which is what waitNext reads: an individual feed ending
// only means a replacement session is about to re-attach.
func newWaitNextProgress() (*uploadProgress, context.CancelFunc) {
	ctx, cancel := context.WithCancel(context.Background())
	return &uploadProgress{ctx: ctx, cancel: cancel, done: make(chan struct{}), changed: make(chan struct{}, 1)}, cancel
}

func TestUploadProgressWaitNext(t *testing.T) {
	t.Run("already advanced", func(t *testing.T) {
		progress, cancel := newWaitNextProgress()
		defer cancel()
		progress.seq.Store(2)
		if !progress.waitNext(context.Background(), 1) {
			t.Fatal("waitNext rejected an available update")
		}
	})

	t.Run("notification", func(t *testing.T) {
		progress, cancel := newWaitNextProgress()
		defer cancel()
		result := make(chan bool, 1)
		go func() { result <- progress.waitNext(context.Background(), 0) }()
		progress.seq.Store(1)
		progress.changed <- struct{}{}
		if !<-result {
			t.Fatal("waitNext ignored a progress edge")
		}
	})

	t.Run("cancellation", func(t *testing.T) {
		progress, cancel := newWaitNextProgress()
		defer cancel()
		ctx, cancelCaller := context.WithCancel(context.Background())
		cancelCaller()
		if progress.waitNext(ctx, 0) {
			t.Fatal("waitNext succeeded after cancellation")
		}
	})

	t.Run("terminal", func(t *testing.T) {
		progress, cancel := newWaitNextProgress()
		cancel()
		if progress.waitNext(context.Background(), 0) {
			t.Fatal("waitNext succeeded after the progress channel closed")
		}
	})

	t.Run("final update", func(t *testing.T) {
		progress, cancel := newWaitNextProgress()
		cancel()
		progress.seq.Store(1)
		if !progress.waitNext(context.Background(), 0) {
			t.Fatal("waitNext dropped the final progress update")
		}
	})

	// A feed replaced mid-stage closes its own reader. Treating that as the end
	// of the report would fail the stage on every session rollover.
	t.Run("feed replaced", func(t *testing.T) {
		progress, cancel := newWaitNextProgress()
		defer cancel()
		result := make(chan bool, 1)
		go func() { result <- progress.waitNext(context.Background(), 0) }()
		progress.mu.Lock()
		close(progress.done)
		progress.done = make(chan struct{})
		progress.mu.Unlock()
		select {
		case <-result:
			t.Fatal("waitNext ended the report when one feed was replaced")
		case <-time.After(200 * time.Millisecond):
		}
		progress.seq.Store(1)
		progress.changed <- struct{}{}
		if !<-result {
			t.Fatal("waitNext missed the replacement feed's update")
		}
	})
}

// Two readers drain the same aggregate at once: the HTTP re-attach and a
// replacement session's re-attached stream. Under a check followed by a store,
// the older of two interleaved records lands last and walks the
// server-authoritative counter backwards, or splits a byte total from the
// active time it was measured over. Both under-report the final window.
func TestUploadProgressCounterHoldsUnderConcurrentFeeds(t *testing.T) {
	p := &uploadProgress{ready: make(chan error, 1), changed: make(chan struct{}, 1)}
	const feeds, records = 4, 4000

	// Every record pairs a byte total with an equal nanosecond stamp, so an
	// observer can see both a regression and a torn pair.
	stop := make(chan struct{})
	fault := make(chan string, 1)
	watching := make(chan struct{})
	go func() {
		close(watching)
		var last uint64
		for {
			select {
			case <-stop:
				return
			default:
			}
			bytes, nanos := p.counters()
			if bytes < last {
				fault <- fmt.Sprintf("counter went backwards: %d then %d", last, bytes)
				return
			}
			if bytes != nanos {
				fault <- fmt.Sprintf("torn counter pair: %d bytes beside %d nanos", bytes, nanos)
				return
			}
			last = bytes
			runtime.Gosched()
		}
	}()
	<-watching

	var wg sync.WaitGroup
	for range feeds {
		body, feed := io.Pipe()
		done := make(chan struct{})
		wg.Go(func() { p.read(body, done) })
		wg.Go(func() {
			defer feed.Close() //nolint:errcheck // the reader's own error path covers this
			for i := uint64(1); i <= records; i++ {
				if _, err := fmt.Fprintf(feed, "{\"type\":\"progress\",\"bytes\":%d,\"nanos\":%d}\n", i, i); err != nil {
					return
				}
			}
		})
	}
	wg.Wait()
	close(stop)

	select {
	case reason := <-fault:
		t.Fatal(reason)
	default:
	}
	if bytes, nanos := p.counters(); bytes != records || nanos != records {
		t.Fatalf("final counter = (%d, %d), want (%d, %d)", bytes, nanos, records, records)
	}
}
