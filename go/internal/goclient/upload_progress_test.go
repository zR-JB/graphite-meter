package goclient

import (
	"context"
	"encoding/json/jsontext"
	jsonv2 "encoding/json/v2"
	"errors"
	"fmt"
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

	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

func testUploadFeed(body io.ReadCloser) *uploadFeed {
	return &uploadFeed{ReadCloser: body, interrupt: func() { _ = body.Close() }}
}

func newWaitNextProgress(t *testing.T) (*uploadProgress, context.CancelFunc) {
	ctx, cancel := context.WithCancel(t.Context())
	return &uploadProgress{ctx: ctx, cancel: cancel, done: make(chan struct{}), changed: make(chan struct{}, 1), errs: make(chan error, 1)}, cancel
}

// The receiver's (bytes, nanos) pair only moves forward: regressions, malformed records, superseded feeds,
// and racing feeds can never walk it back or tear it.
func TestUploadProgressKeepsAForwardPair(t *testing.T) {
	t.Parallel()
	t.Run("records", func(t *testing.T) {
		t.Parallel()
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
		p.read(testUploadFeed(io.NopCloser(body)), make(chan struct{}))
		if n, ns := p.counters(); n != 120 || ns != 30 || p.seq.Load() != 2 {
			t.Fatalf("accepted receiver pair = (%d, %d), sequence=%d", n, ns, p.seq.Load())
		}
		// An explicit zero window is evidence, not a missing value.
		zero := &uploadProgress{ready: make(chan error, 1), changed: make(chan struct{}, 1)}
		zero.read(testUploadFeed(io.NopCloser(strings.NewReader("{\"type\":\"ready\"}\n{\"type\":\"complete\",\"bytes\":0,\"nanos\":0}\n"))), make(chan struct{}))
		if n, ns := zero.counters(); n != 0 || ns != 0 || zero.seq.Load() != 1 {
			t.Fatalf("zero window = (%d, %d), sequence=%d", n, ns, zero.seq.Load())
		}
	})
	t.Run("superseded feed", func(t *testing.T) {
		t.Parallel()
		live, liveWriter := io.Pipe()
		go func() {
			defer liveWriter.Close()
			enc := jsontext.NewEncoder(liveWriter)
			_ = jsonv2.MarshalEncode(enc, wire.UploadProgress{Type: "ready"})
			_ = jsonv2.MarshalEncode(enc, wire.UploadProgress{Type: "complete", Bytes: 1000, Nanos: uint64(5 * time.Second)})
		}()
		r := &runner{emit: func(Event) {}}
		p, err := r.readUploadProgress(t.Context(), testUploadFeed(live), "http://127.0.0.1/upload/progress")
		if err != nil {
			t.Fatalf("readUploadProgress: %v", err)
		}
		defer p.close()
		if err := p.waitNext(t.Context(), 0, nil); err != nil {
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
		if bytes, nanos := p.counters(); bytes != 1000 || nanos != uint64(5*time.Second) {
			t.Fatalf("counters = (%d bytes, %v), want (1000 bytes, 5s): the superseded feed walked the pair backwards", bytes, time.Duration(nanos))
		}
	})
	t.Run("concurrent feeds", func(t *testing.T) {
		t.Parallel()
		p := &uploadProgress{ready: make(chan error, 1), changed: make(chan struct{}, 1)}
		const feeds, records = 4, 1000
		stop, fault := make(chan struct{}), make(chan string, 1)
		var watcher sync.WaitGroup
		watcher.Go(func() {
			var last uint64
			for {
				select {
				case <-stop:
					return
				default:
				}
				bytes, nanos := p.counters()
				if bytes < last || bytes != nanos {
					fault <- fmt.Sprintf("counter went from %d to (%d bytes, %d nanos)", last, bytes, nanos)
					return
				}
				last = bytes
				runtime.Gosched()
			}
		})
		var wg sync.WaitGroup
		for range feeds {
			body, feed := io.Pipe()
			wg.Go(func() { p.read(testUploadFeed(body), make(chan struct{})) })
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
		watcher.Wait()
		select {
		case reason := <-fault:
			t.Fatal(reason)
		default:
		}
		if bytes, nanos := p.counters(); bytes != records || nanos != records {
			t.Fatalf("final counter = (%d, %d), want (%d, %d)", bytes, nanos, records, records)
		}
	})
}

func TestUploadProgressWaitNext(t *testing.T) {
	t.Parallel()
	failure := errors.New("upload lane rejected")
	for _, c := range []struct {
		name    string
		prepare func(*uploadProgress, context.CancelFunc) (ctx context.Context, lane chan error)
		wantErr string
	}{
		{"lane failure first", func(*uploadProgress, context.CancelFunc) (context.Context, chan error) {
			lane := make(chan error, 1)
			lane <- failure
			return t.Context(), lane
		}, failure.Error()},
		{"already advanced", func(p *uploadProgress, _ context.CancelFunc) (context.Context, chan error) {
			p.seq.Store(2)
			return t.Context(), nil
		}, ""},
		{"caller cancelled", func(*uploadProgress, context.CancelFunc) (context.Context, chan error) {
			ctx, cancel := context.WithCancel(t.Context())
			cancel()
			return ctx, nil
		}, "canceled"},
		{"feed ended", func(_ *uploadProgress, cancel context.CancelFunc) (context.Context, chan error) {
			cancel()
			return t.Context(), nil
		}, "did not advance"},
		{"feed died for good", func(p *uploadProgress, cancel context.CancelFunc) (context.Context, chan error) {
			p.errs <- errors.New("upload progress lost and not reattached within 2s")
			cancel()
			return t.Context(), nil
		}, "not reattached"},
	} {
		progress, cancel := newWaitNextProgress(t)
		ctx, lane := c.prepare(progress, cancel)
		err := progress.waitNext(ctx, 1, lane)
		if c.wantErr == "" && err != nil || c.wantErr != "" && (err == nil || !strings.Contains(err.Error(), c.wantErr)) {
			t.Errorf("%s: waitNext = %v, want %q", c.name, err, c.wantErr)
		}
		cancel()
	}
	// A waiter wakes on a progress edge and survives one feed replacing another; a final update beats the feed's end.
	synctest.Test(t, func(t *testing.T) {
		progress, cancel := newWaitNextProgress(t)
		defer cancel()
		result := make(chan error, 1)
		go func() { result <- progress.waitNext(t.Context(), 0, nil) }()
		synctest.Wait()
		progress.mu.Lock()
		close(progress.done)
		progress.done = make(chan struct{})
		progress.mu.Unlock()
		synctest.Wait()
		select {
		case <-result:
			t.Fatal("waitNext ended the report when one feed was replaced")
		default:
		}
		progress.seq.Store(1)
		progress.changed <- struct{}{}
		if err := <-result; err != nil {
			t.Fatal("waitNext missed the replacement feed's update")
		}
		final, cancelFinal := newWaitNextProgress(t)
		go func() { result <- final.waitNext(t.Context(), 0, nil) }()
		synctest.Wait()
		final.seq.Store(1)
		cancelFinal()
		if err := <-result; err != nil {
			t.Fatal("waitNext dropped the final progress update")
		}
	})
}

// One progress response is bounded by its request; the reattach resumes the same aggregate at a paced rate.
func TestReattachUploadProgressResumesTheSameAggregate(t *testing.T) {
	t.Parallel()
	const recordsPerFeed = 3
	const window = 1200 * time.Millisecond
	var gets, served atomic.Int64
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		gets.Add(1)
		enc := jsontext.NewEncoder(w)
		_ = jsonv2.MarshalEncode(enc, wire.UploadProgress{Type: "ready"})
		w.(http.Flusher).Flush()
		for range recordsPerFeed {
			n := uint64(served.Add(1))
			_ = jsonv2.MarshalEncode(enc, wire.UploadProgress{Type: "progress", Bytes: n, Nanos: n})
			w.(http.Flusher).Flush()
		}
	}))
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
		t.Errorf("the counter stopped at %d, want it past %d: the reattach resumes the same aggregate", carried, recordsPerFeed)
	}
	if paced := int64(window/wtRedialBackoff) + 2; gets.Load() > paced {
		t.Errorf("issued %d progress GETs in %v, want at most %d: the reopen is not paced", gets.Load(), window, paced)
	}
}

// A permanent refusal ends recovery with its cause after one request.
func TestUploadProgressPermanentLossFails(t *testing.T) {
	t.Parallel()
	progress, cancel := newWaitNextProgress(t)
	defer cancel()
	progress.count.Store(&uploadCount{bytes: 200, nanos: uint64(2 * time.Second)})
	close(progress.done)
	var requests atomic.Int64
	r := &runner{http: &http.Client{Transport: roundTripFunc(func(req *http.Request) (*http.Response, error) {
		requests.Add(1)
		header := http.Header{"Graphite-Meter-Auth": {"required"}, "Graphite-Meter-Auth-Url": {"/auth/start"}}
		return &http.Response{StatusCode: http.StatusForbidden, Header: header, Body: io.NopCloser(strings.NewReader("")), Request: req}, nil
	})}, emit: func(Event) {}}
	go r.reattachUploadProgress(progress, "http://progress.invalid/upload/progress")
	err := waitCoordinatedTransfer(t.Context(), nil, progress.errs)
	if _, ok := errors.AsType[*AuthRequiredError](err); !ok || requests.Load() != 1 {
		t.Fatalf("permanent auth refusal = %v after %d requests, want AuthRequiredError after one", err, requests.Load())
	}
}

type ownedProgressBody struct {
	started         chan struct{}
	stop            chan struct{}
	release         chan struct{} // Nil closes at once; otherwise Close waits for it.
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
	if b.release != nil {
		<-b.release
	}
	b.concurrentClose.Store(b.reading.Load())
	b.closed.Store(true)
	return nil
}

type closeRecorder struct {
	io.Reader
	closed atomic.Bool
}

func (c *closeRecorder) Close() error {
	c.closed.Store(true)
	return nil
}

// Closing progress joins every reader it ever adopted and any recovery in flight, and adopts nothing afterwards.
func TestUploadProgressCloseJoinsReadersAndRecovery(t *testing.T) {
	t.Parallel()
	t.Run("reader owns its body", func(t *testing.T) {
		synctest.Test(t, func(t *testing.T) {
			body := &ownedProgressBody{started: make(chan struct{}), stop: make(chan struct{})}
			progress, err := (&runner{}).readUploadProgress(t.Context(), &uploadFeed{ReadCloser: body, interrupt: sync.OnceFunc(func() { close(body.stop) })}, "")
			if err != nil {
				t.Fatal(err)
			}
			<-body.started
			progress.close()
			if body.concurrentClose.Load() || !body.closed.Load() {
				t.Fatalf("reader close ownership: concurrent=%v closed=%v", body.concurrentClose.Load(), body.closed.Load())
			}
			late := &closeRecorder{Reader: strings.NewReader("{\"type\":\"progress\",\"bytes\":1,\"nanos\":1}\n")}
			progress.attach(testUploadFeed(late))
			if bytes, _ := progress.counters(); !late.closed.Load() || bytes != 0 {
				t.Fatalf("a reader offered after close was adopted: closed=%v bytes=%d", late.closed.Load(), bytes)
			}
		})
	})
	t.Run("superseded reader", func(t *testing.T) {
		synctest.Test(t, func(t *testing.T) {
			body := &ownedProgressBody{started: make(chan struct{}), stop: make(chan struct{}), release: make(chan struct{})}
			progress, err := (&runner{http: &http.Client{}}).readUploadProgress(t.Context(), &uploadFeed{ReadCloser: body, interrupt: sync.OnceFunc(func() { close(body.stop) })}, "http://fixture.invalid/upload/progress")
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
				t.Fatal("cleanup returned while the superseded reader still owned its body")
			default:
			}
			close(body.release)
			<-done
			if !body.closed.Load() {
				t.Fatal("superseded reader was not joined")
			}
		})
	})
	t.Run("recovery", func(t *testing.T) {
		synctest.Test(t, func(t *testing.T) {
			started, release := make(chan struct{}), make(chan struct{})
			var requests int
			var active atomic.Bool
			r := &runner{http: &http.Client{Transport: roundTripFunc(func(req *http.Request) (*http.Response, error) {
				if requests++; requests == 1 {
					return &http.Response{StatusCode: http.StatusOK, Body: io.NopCloser(strings.NewReader("{\"type\":\"ready\"}\n")), Request: req}, nil
				}
				active.Store(true)
				defer active.Store(false)
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
				t.Fatal("cleanup returned while recovery still owned its request")
			default:
			}
			close(release)
			<-done
			if active.Load() {
				t.Fatal("recovery request was not joined")
			}
		})
	})
}
