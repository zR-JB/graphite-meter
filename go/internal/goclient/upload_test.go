package goclient

import (
	"bytes"
	"context"
	"encoding/json/jsontext"
	jsonv2 "encoding/json/v2"
	"errors"
	"fmt"
	"github.com/zR-JB/graphite-meter/go/internal/wire"
	"io"
	"net/http"
	"net/http/httptest"
	"runtime"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"testing/synctest"
	"time"
)

func TestCyclingBodyWrapsDeterministically(t *testing.T) {
	block := []byte{1, 2, 3, 4, 5}
	b := &cyclingBody{ctx: t.Context(), block: block}

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
	b := &cyclingBody{ctx: t.Context(), block: []byte{1, 2, 3}, limit: 7}
	var got []byte
	buf := make([]byte, 4)
	for {
		n, err := b.Read(buf)
		got = append(got, buf[:n]...)
		if errors.Is(err, io.EOF) {
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
	ctx, cancel := context.WithCancel(t.Context())
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
			_ = jsonv2.MarshalWrite(w, uploadSessionResponse{UploadID: "abc-123"})
		}))
		defer srv.Close()
		r := &runner{cfg: Config{BaseURL: srv.URL}, http: srv.Client()}
		id, err := r.mintUploadID(t.Context())
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
		if _, err := r.mintUploadID(t.Context()); err == nil {
			t.Fatal("want an error for a non-200 upload session response")
		}
	})

	t.Run("empty uploadId is an error", func(t *testing.T) {
		srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
			w.Header().Set("Content-Type", "application/json")
			_ = jsonv2.MarshalWrite(w, uploadSessionResponse{})
		}))
		defer srv.Close()
		r := &runner{cfg: Config{BaseURL: srv.URL}, http: srv.Client()}
		if _, err := r.mintUploadID(t.Context()); err == nil {
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
		if _, err := r.mintUploadID(t.Context()); err == nil {
			t.Fatal("want an error for a malformed session response")
		}
	})
}

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

	ctx, cancel := context.WithCancel(t.Context())
	done := make(chan struct{})
	go func() {
		_ = r.uploadLane(ctx, "test-id", 0, block, func() {})
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
	if err := r.uploadLane(t.Context(), "test-id", 0, make([]byte, 1024), func() {}); err == nil {
		t.Fatal("HTTP 503 did not fail the upload lane")
	}
}

func newAbruptCloseUploadServer(requests *atomic.Int64) *httptest.Server {
	return httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		requests.Add(1)
		buf := make([]byte, 8*1024)
		_, _ = r.Body.Read(buf)
		panic(http.ErrAbortHandler)
	}))
}

func TestUploadLaneSurvivesAbruptConnectionDrop(t *testing.T) {
	var requests atomic.Int64
	srv := newAbruptCloseUploadServer(&requests)
	defer srv.Close()

	r := &runner{cfg: Config{BaseURL: srv.URL}, http: srv.Client()}
	block := make([]byte, 64*1024)

	ctx, cancel := context.WithCancel(t.Context())
	done := make(chan struct{})
	go func() {
		_ = r.uploadLane(ctx, "test-id", 0, block, func() {})
		close(done)
	}()

	defer cancel()
	deadline := time.After(2 * time.Second)
	for requests.Load() < 2 {
		select {
		case <-done:
			t.Fatal("upload lane stopped instead of retrying the dropped connection")
		case <-deadline:
			t.Fatal("upload lane did not reopen after the dropped connection")
		case <-time.After(5 * time.Millisecond):
		}
	}
	cancel()
	select {
	case <-done:
	case <-time.After(2 * time.Second):
		t.Fatal("uploadLane did not return after repeated abrupt connection drops plus cancellation")
	}
}

func mountFakeProgress(mux *http.ServeMux, served *atomic.Uint64, started time.Time) {
	mux.HandleFunc("/upload/checkpoint", func(w http.ResponseWriter, r *http.Request) {
		_ = jsonv2.MarshalWrite(w, struct {
			Bytes uint64 `json:"bytes"`
			Nanos uint64 `json:"nanos"`
		}{served.Load(), uint64(time.Since(started))})
	})
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
		enc := jsontext.NewEncoder(w)
		_ = jsonv2.MarshalEncode(enc, wire.UploadProgress{Type: "ready"})
		flusher.Flush()
		ticker := time.Tick(20 * time.Millisecond)
		for {
			select {
			case <-r.Context().Done():
				return
			case <-finished:
				_ = jsonv2.MarshalEncode(enc, wire.UploadProgress{Type: "complete", Bytes: served.Load(), Nanos: uint64(time.Since(started))})
				flusher.Flush()
				return
			case <-ticker:
				_ = jsonv2.MarshalEncode(enc, wire.UploadProgress{Type: "progress", Bytes: served.Load(), Nanos: uint64(time.Since(started))})
				flusher.Flush()
			}
		}
	})
}

func newFakeUploadServer(t *testing.T) *httptest.Server {
	t.Helper()
	var served atomic.Uint64
	started := time.Now()

	mux := http.NewServeMux()
	mux.HandleFunc("/upload/session", func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		_ = jsonv2.MarshalWrite(w, uploadSessionResponse{UploadID: "test-upload"})
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

	ctx, cancel := context.WithTimeout(t.Context(), 5*time.Second)
	defer cancel()

	res, err := r.testTransferResult(ctx, "upload", 300*time.Millisecond)
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

func newStalledUploadServer() *httptest.Server {
	started := time.Now()
	mux := http.NewServeMux()
	mux.HandleFunc("/upload/checkpoint", func(w http.ResponseWriter, _ *http.Request) {
		_, _ = fmt.Fprintf(w, `{"bytes":0,"nanos":%d}`, time.Since(started))
	})
	mux.HandleFunc("/upload/session", func(w http.ResponseWriter, _ *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		_ = jsonv2.MarshalWrite(w, uploadSessionResponse{UploadID: "stalled-upload"})
	})
	mux.HandleFunc("/upload", func(_ http.ResponseWriter, r *http.Request) {
		_, _ = io.Copy(io.Discard, r.Body)
	})
	mux.HandleFunc("/upload/progress", func(w http.ResponseWriter, r *http.Request) {
		if r.Method == http.MethodDelete {
			w.WriteHeader(http.StatusNoContent)
			return
		}
		w.Header().Set("Content-Type", "application/x-ndjson")
		flusher := w.(http.Flusher)
		enc := jsontext.NewEncoder(w)
		_ = jsonv2.MarshalEncode(enc, wire.UploadProgress{Type: "ready"})
		flusher.Flush()
		ticker := time.Tick(20 * time.Millisecond)
		for {
			select {
			case <-r.Context().Done():
				return
			case <-ticker:
				_ = jsonv2.MarshalEncode(enc, wire.UploadProgress{Type: "progress", Bytes: 0, Nanos: uint64(time.Since(started))})
				flusher.Flush()
			}
		}
	})
	return httptest.NewServer(mux)
}

func TestMeasureUploadCancelledEmptyWindowIsACleanStop(t *testing.T) {
	srv := newStalledUploadServer()
	defer srv.Close()

	cfg := Config{BaseURL: srv.URL, TransferStreams: TransferStreamPolicy{Forced: 1}}.normalized()
	r := &runner{cfg: cfg, streams: streamCounts{down: 1, up: 1}, http: srv.Client(), emit: func(Event) {}}
	attachTestLatencyTarget(r, srv.URL)

	ctx, cancel := context.WithCancel(t.Context())
	defer cancel()
	time.AfterFunc(200*time.Millisecond, cancel)

	_, err := r.testTransferResult(ctx, "upload", 5*time.Second)
	if !errors.Is(err, context.Canceled) {
		t.Fatalf("cancelled stage returned %v, want context.Canceled", err)
	}
	if strings.Contains(err.Error(), "carried no bytes") {
		t.Errorf("err = %v, want the cancellation reported as a stop", err)
	}
}

func TestUploadProgressHoldsTheForwardPairAcrossFeeds(t *testing.T) {
	live, liveWriter := io.Pipe()
	go func() {
		defer liveWriter.Close()
		enc := jsontext.NewEncoder(liveWriter)
		_ = jsonv2.MarshalEncode(enc, wire.UploadProgress{Type: "ready"})
		_ = jsonv2.MarshalEncode(enc, wire.UploadProgress{Type: "complete", Bytes: 1000, Nanos: uint64(5 * time.Second)})
	}()

	ctx, cancel := context.WithTimeout(t.Context(), 5*time.Second)
	defer cancel()
	r := &runner{cfg: DefaultConfig(), emit: func(Event) {}}
	p, err := r.readUploadProgress(ctx, testUploadFeed(live), "http://127.0.0.1/upload/progress")
	if err != nil {
		t.Fatalf("readUploadProgress: %v", err)
	}
	defer p.close()
	if err := p.waitNext(ctx, 0, nil); err != nil {
		t.Fatal("the live feed never published a count")
	}

	stale, staleWriter := io.Pipe()
	p.attach(testUploadFeed(stale))
	if err := jsonv2.MarshalEncode(jsontext.NewEncoder(staleWriter), wire.UploadProgress{Type: "progress", Bytes: 1000, Nanos: uint64(3200 * time.Millisecond)}); err != nil {
		t.Fatalf("write the superseded feed's buffered record: %v", err)
	}
	staleWriter.Close()
	_, done := p.current()
	<-done

	bytes, nanos := p.counters()
	if bytes != 1000 || nanos != uint64(5*time.Second) {
		t.Fatalf("counters = (%d bytes, %v), want (1000 bytes, 5s): the superseded feed walked the pair backwards", bytes, time.Duration(nanos))
	}
}

func newWaitNextProgress(t *testing.T) (*uploadProgress, context.CancelFunc) {
	ctx, cancel := context.WithCancel(t.Context())
	return &uploadProgress{ctx: ctx, cancel: cancel, done: make(chan struct{}), changed: make(chan struct{}, 1), errs: make(chan error, 1)}, cancel
}

func TestUploadProgressWaitNext(t *testing.T) {
	t.Run("lane failure", func(t *testing.T) {
		progress, cancel := newWaitNextProgress(t)
		defer cancel()
		laneErr := make(chan error, 1)
		failure := errors.New("upload lane rejected")
		laneErr <- failure
		if err := progress.waitNext(t.Context(), 0, laneErr); !errors.Is(err, failure) {
			t.Fatalf("waitNext = %v, want lane failure before the first receiver counter", err)
		}
	})

	t.Run("already advanced", func(t *testing.T) {
		progress, cancel := newWaitNextProgress(t)
		defer cancel()
		progress.seq.Store(2)
		if err := progress.waitNext(t.Context(), 1, nil); err != nil {
			t.Fatal("waitNext rejected an available update")
		}
	})

	t.Run("notification", func(t *testing.T) {
		progress, cancel := newWaitNextProgress(t)
		defer cancel()
		result := make(chan error, 1)
		go func() { result <- progress.waitNext(t.Context(), 0, nil) }()
		progress.seq.Store(1)
		progress.changed <- struct{}{}
		if err := <-result; err != nil {
			t.Fatal("waitNext ignored a progress edge")
		}
	})

	t.Run("cancellation", func(t *testing.T) {
		progress, cancel := newWaitNextProgress(t)
		defer cancel()
		ctx, cancelCaller := context.WithCancel(t.Context())
		cancelCaller()
		if err := progress.waitNext(ctx, 0, nil); err == nil {
			t.Fatal("waitNext succeeded after cancellation")
		}
	})

	t.Run("terminal", func(t *testing.T) {
		progress, cancel := newWaitNextProgress(t)
		cancel()
		if err := progress.waitNext(t.Context(), 0, nil); err == nil {
			t.Fatal("waitNext succeeded after the progress channel closed")
		}
	})

	t.Run("final update", func(t *testing.T) {
		synctest.Test(t, func(t *testing.T) {
			progress, cancel := newWaitNextProgress(t)
			result := make(chan error, 1)
			go func() { result <- progress.waitNext(t.Context(), 0, nil) }()
			synctest.Wait() // the waiter is parked with nothing published
			progress.seq.Store(1)
			cancel()
			if err := <-result; err != nil {
				t.Fatal("waitNext dropped the final progress update")
			}
		})
	})

	t.Run("feed replaced", func(t *testing.T) {
		progress, cancel := newWaitNextProgress(t)
		defer cancel()
		result := make(chan error, 1)
		go func() { result <- progress.waitNext(t.Context(), 0, nil) }()
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
		if err := <-result; err != nil {
			t.Fatal("waitNext missed the replacement feed's update")
		}
	})
}

func TestUploadProgressCounterHoldsUnderConcurrentFeeds(t *testing.T) {
	p := &uploadProgress{ready: make(chan error, 1), changed: make(chan struct{}, 1)}
	const feeds, records = 4, 4000

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
		wg.Go(func() { p.read(testUploadFeed(body), done) })
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

// closeRecorder is a feed body that reports whether the reader that adopted it released it.
type closeRecorder struct {
	io.Reader
	closed atomic.Bool
}

func (c *closeRecorder) Close() error {
	c.closed.Store(true)
	return nil
}

func TestReattachUploadProgressResumesTheSameAggregate(t *testing.T) {
	const recordsPerFeed = 3
	const window = 1500 * time.Millisecond
	var gets, served atomic.Int64

	mux := http.NewServeMux()
	mux.HandleFunc("/upload/progress", func(w http.ResponseWriter, _ *http.Request) {
		gets.Add(1)
		w.Header().Set("Content-Type", "application/x-ndjson")
		flusher := w.(http.Flusher)
		enc := jsontext.NewEncoder(w)
		_ = jsonv2.MarshalEncode(enc, wire.UploadProgress{Type: "ready"})
		flusher.Flush()
		for range recordsPerFeed {
			n := uint64(served.Add(1))
			_ = jsonv2.MarshalEncode(enc, wire.UploadProgress{Type: "progress", Bytes: n, Nanos: n})
			flusher.Flush()
		}
		// The handler returns: the request's bound, not the aggregate's.
	})
	srv := httptest.NewServer(mux)
	defer srv.Close()

	r := &runner{cfg: Config{BaseURL: srv.URL}.normalized(), http: srv.Client(), emit: func(Event) {}}
	ctx, cancel := context.WithTimeout(t.Context(), window)
	defer cancel()
	p, err := r.openUploadProgress(ctx, srv.URL+"/upload/progress")
	if err != nil {
		t.Fatalf("openUploadProgress: %v", err)
	}
	defer p.close()

	<-ctx.Done()
	if carried, _ := p.counters(); carried <= recordsPerFeed {
		t.Errorf("the counter stopped at %d, want it past %d: one feed cannot carry the stage, and the reattach resumes the same aggregate", carried, recordsPerFeed)
	}
	if paced := int64(window/wtRedialBackoff) + 2; gets.Load() > paced {
		t.Errorf("issued %d progress GETs in %v, want at most %d: the reopen is not paced", gets.Load(), window, paced)
	}
}

func TestAttachRefusesAReaderAfterClose(t *testing.T) {
	progress, cancel := newWaitNextProgress(t)
	_, before := progress.current()
	cancel()

	late := &closeRecorder{Reader: strings.NewReader("{\"type\":\"progress\",\"bytes\":1,\"nanos\":1}\n")}
	progress.attach(testUploadFeed(late))

	if !late.closed.Load() {
		t.Error("the reader offered after the report ended was adopted instead of released")
	}
	if _, done := progress.current(); done != before {
		t.Error("attach installed a reader behind the closed feed")
	}
	if bytes, _ := progress.counters(); bytes != 0 {
		t.Errorf("the late reader's records reached the counter (%d bytes)", bytes)
	}
}

func TestUploadProgressPermanentLossRejectsAStalePrefix(t *testing.T) {
	progress, cancel := newWaitNextProgress(t)
	defer cancel()
	progress.count.Store(&uploadCount{bytes: 200, nanos: uint64(2 * time.Second)})
	close(progress.done)

	var requests atomic.Int64
	r := &runner{cfg: DefaultConfig(), http: &http.Client{Transport: roundTripFunc(func(req *http.Request) (*http.Response, error) {
		requests.Add(1)
		return &http.Response{
			StatusCode: http.StatusForbidden,
			Header: http.Header{
				"Graphite-Meter-Auth":     {"required"},
				"Graphite-Meter-Auth-URL": {"/auth/start"},
			},
			Body: io.NopCloser(strings.NewReader("")), Request: req,
		}, nil
	})}, emit: func(Event) {}}
	go r.reattachUploadProgress(progress, "http://progress.invalid/upload/progress")
	err := waitCoordinatedTransfer(t.Context(), nil, progress.errs)
	if _, ok := errors.AsType[*AuthRequiredError](err); !ok {
		t.Fatalf("permanent auth refusal = %v, want AuthRequiredError", err)
	}
	if got := requests.Load(); got != 1 {
		t.Fatalf("permanent auth refusal made %d requests, want one without retries", got)
	}
}

func TestUploadProgressWaitNextEndsWhenTheFeedDiesForGood(t *testing.T) {
	ctx := t.Context()
	readCtx, readCancel := context.WithCancel(ctx)
	defer readCancel()
	p := &uploadProgress{ctx: readCtx, cancel: readCancel, ready: make(chan error, 1), changed: make(chan struct{}, 1), errs: make(chan error, 1)}

	go func() {
		p.errs <- errors.New("upload progress lost and not reattached within 2s: context deadline exceeded")
		p.cancel()
	}()

	done := make(chan error, 1)
	go func() { done <- p.waitNext(ctx, p.seq.Load(), nil) }()
	select {
	case err := <-done:
		if err == nil || !strings.Contains(err.Error(), "not reattached") {
			t.Fatalf("waitNext = %v, want the feed's own failure", err)
		}
	case <-time.After(5 * time.Second):
		t.Fatal("waitNext never returned: a dead feed hangs the upload stage")
	}
}

func testUploadFeed(body io.ReadCloser) *uploadFeed {
	return &uploadFeed{ReadCloser: body, interrupt: func() { _ = body.Close() }}
}

type ownedProgressBody struct {
	started         chan struct{}
	stop            chan struct{}
	first           bool
	reading         atomic.Bool
	concurrentClose atomic.Bool
	closed          atomic.Bool
}

func (b *ownedProgressBody) Read(p []byte) (int, error) {
	if !b.first {
		b.first = true
		return copy(p, "{\"type\":\"ready\"}\n"), nil
	}
	b.reading.Store(true)
	close(b.started)
	<-b.stop
	b.reading.Store(false)
	return 0, io.EOF
}

func (b *ownedProgressBody) Close() error {
	b.concurrentClose.Store(b.reading.Load())
	b.closed.Store(true)
	return nil
}

func TestUploadFeedReaderOwnsBodyCloseAfterInterruption(t *testing.T) {
	body := &ownedProgressBody{started: make(chan struct{}), stop: make(chan struct{})}
	feed := &uploadFeed{ReadCloser: body, interrupt: sync.OnceFunc(func() { close(body.stop) })}
	r := &runner{}
	progress, err := r.readUploadProgress(t.Context(), feed, "")
	if err != nil {
		t.Fatal(err)
	}
	<-body.started
	done := make(chan struct{})
	go func() { progress.close(); close(done) }()
	select {
	case <-done:
	case <-time.After(time.Second):
		t.Fatal("feed cancellation did not join its reader")
	}
	if body.concurrentClose.Load() || !body.closed.Load() {
		t.Fatalf("reader close ownership: concurrent=%v closed=%v", body.concurrentClose.Load(), body.closed.Load())
	}
}

func TestUploadProgressRejectsMalformedAndRegressedPairs(t *testing.T) {
	p := &uploadProgress{ready: make(chan error, 1), changed: make(chan struct{}, 1)}
	body := strings.NewReader(strings.Join([]string{
		`{"type":"ready"}`,
		`{"type":"progress","bytes":100,"nanos":10}`,
		`{"type":"progress","bytes":90,"nanos":20}`,
		`{"type":"progress","bytes":110,"nanos":9}`,
		`{"type":"complete","bytes":200}`,
		`{"type":"complete","bytes":"200","nanos":20}`,
		`{"type":"complete","bytes":200,"nanos":9}`,
		`{"type":"progress","bytes":120,"nanos":30}`,
		"",
	}, "\n"))
	done := make(chan struct{})
	p.read(testUploadFeed(io.NopCloser(body)), done)
	if n, ns := p.counters(); n != 120 || ns != 30 || p.seq.Load() != 2 {
		t.Fatalf("accepted receiver pair = (%d, %d), sequence=%d", n, ns, p.seq.Load())
	}
}

func TestUploadProgressPreservesExplicitZeroWindow(t *testing.T) {
	p := &uploadProgress{ready: make(chan error, 1), changed: make(chan struct{}, 1)}
	body := strings.NewReader("{\"type\":\"ready\"}\n{\"type\":\"complete\",\"bytes\":0,\"nanos\":0}\n")
	p.read(testUploadFeed(io.NopCloser(body)), make(chan struct{}))
	if n, ns := p.counters(); n != 0 || ns != 0 || p.seq.Load() != 1 {
		t.Fatalf("zero window = (%d, %d), sequence=%d", n, ns, p.seq.Load())
	}
}

// Closing an old reader may finish after its replacement is already reading.
type delayedCloseProgressBody struct {
	ownedProgressBody
	release chan struct{}
}

func (b *delayedCloseProgressBody) Close() error {
	<-b.release
	return b.ownedProgressBody.Close()
}

func TestUploadProgressCloseJoinsSupersededReaders(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		body := &delayedCloseProgressBody{
			started: make(chan struct{}),
			stop:    make(chan struct{}),
			release: make(chan struct{}),
		}
		feed := &uploadFeed{ReadCloser: body, interrupt: sync.OnceFunc(func() { close(body.stop) })}
		r := &runner{http: &http.Client{}}
		progress, err := r.readUploadProgress(t.Context(), feed, "http://fixture.invalid/upload/progress")
		if err != nil {
			t.Fatal(err)
		}
		<-body.started
		progress.attach(testUploadFeed(io.NopCloser(strings.NewReader("{\"type\":\"ready\"}\n"))))
		done := make(chan struct{})
		go func() { progress.close(); close(done) }()
		synctest.Wait()
		select {
		case <-done:
			t.Error("cleanup returned while the superseded reader still owned its body")
		default:
		}
		close(body.release)
		<-done
		synctest.Wait()
		if !body.closed.Load() {
			t.Fatal("superseded reader was not joined")
		}
	})
}

func TestUploadProgressCloseJoinsRecovery(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		started, release := make(chan struct{}), make(chan struct{})
		requests := 0
		active := false
		r := &runner{http: &http.Client{Transport: roundTripFunc(func(req *http.Request) (*http.Response, error) {
			requests++
			if requests == 1 {
				return &http.Response{StatusCode: http.StatusOK, Body: io.NopCloser(strings.NewReader("{\"type\":\"ready\"}\n")), Request: req}, nil
			}
			active = true
			defer func() { active = false }()
			close(started)
			<-req.Context().Done()
			<-release
			return nil, req.Context().Err()
		})}}
		progress, err := r.openUploadProgress(t.Context(), "http://fixture.invalid/upload/progress")
		if err != nil {
			t.Fatal(err)
		}
		<-started
		done := make(chan struct{})
		go func() { progress.close(); close(done) }()
		synctest.Wait()
		select {
		case <-done:
			t.Error("cleanup returned while recovery still owned its request")
		default:
		}
		close(release)
		<-done
		synctest.Wait()
		if active {
			t.Fatal("recovery request was not joined")
		}
	})
}
