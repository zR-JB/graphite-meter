package goclient

import (
	"bytes"
	"context"
	"errors"
	"io"
	"net/http"
	"net/http/httptest"
	"slices"
	"strings"
	"sync/atomic"
	"testing"
	"testing/iotest"
	"testing/synctest"
	"time"
)

func TestDownloadLaneCountsExactBytes(t *testing.T) {
	t.Parallel()
	const size = 256 * 1024
	var requests atomic.Int32
	second := make(chan struct{})
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		switch requests.Add(1) {
		case 1:
			_, _ = w.Write(make([]byte, size))
			return
		case 2:
			close(second)
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
	<-second
	cancel()
	<-done
	if got := total.Load(); got != size {
		t.Errorf("total = %d, want %d with no partial second request counted", got, size)
	}
}

func TestMintUploadID(t *testing.T) {
	t.Parallel()
	for body, want := range map[string]string{
		`{"uploadId":"abc-123"}`: "abc-123",
		`{}`:                     "",
		`{"uploadId":"` + strings.Repeat("a", 8193) + `"}`: "",
	} {
		r := &runner{http: &http.Client{Transport: roundTripFunc(func(req *http.Request) (*http.Response, error) {
			reply := io.NopCloser(strings.NewReader(body))
			return &http.Response{StatusCode: http.StatusOK, Body: reply, Request: req}, nil
		})}, target: fetchTarget("http://meter.test")}
		if id, err := r.mintUploadID(t.Context()); id != want || (err == nil) != (want != "") {
			t.Errorf("session response of %d bytes minted %q, %v; want %q", len(body), id, err, want)
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

func TestLanePersistence(t *testing.T) {
	t.Parallel()
	both := []Direction{Down, Up}
	status := func(code int, header http.Header) func(int, *http.Request) (*http.Response, error) {
		return func(_ int, req *http.Request) (*http.Response, error) {
			return &http.Response{StatusCode: code, Header: header, Body: http.NoBody, Request: req}, nil
		}
	}
	refused := func(int, *http.Request) (*http.Response, error) { return nil, errors.New("connection refused") }
	dropped := func(_ int, req *http.Request) (*http.Response, error) {
		if req.Method == http.MethodPost {
			_, _ = io.CopyN(io.Discard, req.Body, 8*1024)
			return nil, io.ErrUnexpectedEOF
		}
		body := io.MultiReader(bytes.NewReader(make([]byte, 64*1024)), iotest.ErrReader(io.ErrUnexpectedEOF))
		return &http.Response{StatusCode: http.StatusOK, Body: io.NopCloser(body), Request: req}, nil
	}
	idle := func(n int, req *http.Request) (*http.Response, error) {
		if n > 1 {
			<-req.Context().Done()
			return nil, req.Context().Err()
		}
		_, _ = io.CopyN(io.Discard, req.Body, 1024)
		return status(http.StatusRequestTimeout, http.Header{"X-Graphite-Upload-Refusal": {"idle"}})(n, req)
	}
	for _, c := range []struct {
		name     string
		dirs     []Direction
		respond  func(n int, req *http.Request) (*http.Response, error)
		window   time.Duration
		reason   FailureReason
		requests int
	}{
		{"busy", both, status(http.StatusTooManyRequests, nil), time.Minute, FailureServerBusy, 4},
		{"unavailable", both, status(http.StatusServiceUnavailable, nil), time.Minute, FailureServerBusy, 4},
		{"gone", both, status(http.StatusGone, nil), time.Minute, FailureProtocol, 1},
		{"unreachable", both, refused, time.Minute, FailureConnectionLost, 5},
		{"empty", []Direction{Down}, status(http.StatusOK, nil), time.Minute, FailureInsufficientEvidence, 5},
		{"dropped mid-transfer", both, dropped, 1250 * time.Millisecond, "", 3},
		{"idle upload redials", []Direction{Up}, idle, time.Second, "", 2},
		{"timed-out upload", []Direction{Up}, status(http.StatusRequestTimeout, nil), time.Minute, FailureProtocol, 1},
	} {
		for _, dir := range c.dirs {
			synctest.Test(t, func(t *testing.T) {
				requests := 0
				transport := roundTripFunc(func(req *http.Request) (*http.Response, error) {
					requests++
					return c.respond(requests, req)
				})
				r := &runner{http: &http.Client{Transport: transport}, target: fetchTarget("http://meter.test")}
				ctx, cancel := context.WithTimeout(t.Context(), c.window)
				defer cancel()
				err := lane(r, dir, "http://meter.test/download")(ctx)
				var reason FailureReason
				if err != nil {
					reason = failureReason(err, false)
				}
				if reason != c.reason || requests != c.requests {
					t.Errorf("%s %s: %v after %d requests, want %q after %d", c.name, dir, err, requests, c.reason,
						c.requests)
				}
			})
		}
	}
}

func TestBusyLaneBacksOffLikeTheBrowser(t *testing.T) {
	t.Parallel()
	const ms = time.Millisecond
	for retryAfter, want := range map[string][]time.Duration{
		"":                              {300 * ms, 600 * ms, 1200 * ms},
		"Wed, 21 Oct 2026 07:28:00 GMT": {300 * ms, 600 * ms, 1200 * ms},
		"1":                             {1000 * ms, 1000 * ms},
		"5":                             {1200 * ms, 1200 * ms},
	} {
		for _, dir := range []Direction{Down, Up} {
			synctest.Test(t, func(t *testing.T) {
				var gaps []time.Duration
				last := time.Now()
				transport := roundTripFunc(func(req *http.Request) (*http.Response, error) {
					if now := time.Now(); now != last {
						gaps = append(gaps, now.Sub(last))
						last = now
					}
					header := http.Header{"Retry-After": {retryAfter}}
					return &http.Response{StatusCode: http.StatusTooManyRequests, Header: header, Body: http.NoBody,
						Request: req}, nil
				})
				r := &runner{http: &http.Client{Transport: transport}, target: fetchTarget("http://meter.test")}
				err := lane(r, dir, "http://meter.test/download")(t.Context())
				if failureReason(err, false) != FailureServerBusy || !slices.Equal(gaps, want) {
					t.Errorf("Retry-After %q %s: %v after gaps %v, want server busy after %v", retryAfter, dir, err, gaps, want)
				}
			})
		}
	}
}

func TestStageCancellationIsPrompt(t *testing.T) {
	t.Parallel()
	for _, c := range []struct {
		name        string
		stage       Stage
		mount       func(*http.ServeMux)
		cancelAfter time.Duration
		warmup      time.Duration
	}{
		{"streaming download", StageDownload, func(mux *http.ServeMux) { mux.HandleFunc("/download", writeDownload) },
			150 * time.Millisecond, 0},
		{"download warmup", StageDownload, func(mux *http.ServeMux) { mux.HandleFunc("/download", writeDownload) },
			150 * time.Millisecond, 3 * time.Second},
		{"silent download", StageDownload, func(mux *http.ServeMux) {
			mux.HandleFunc("/download", func(w http.ResponseWriter, r *http.Request) {
				w.(http.Flusher).Flush()
				<-r.Context().Done()
			})
		}, 150 * time.Millisecond, 0},
		{"already cancelled", StageDownload, func(mux *http.ServeMux) { mux.HandleFunc("/download", writeDownload) },
			0, 0},
		{"stalled upload", StageUpload, func(mux *http.ServeMux) {
			mountUploadReceiver(mux, new(atomic.Uint64), discardUpload)
		}, 200 * time.Millisecond, 0},
	} {
		t.Run(c.name, func(t *testing.T) {
			t.Parallel()
			mux := http.NewServeMux()
			c.mount(mux)
			srv := httptest.NewServer(mux)
			defer srv.Close()
			r := testRunner(srv)
			r.cfg.Warmup = c.warmup
			var log eventLog
			r.emit = log.emit
			ctx, cancel := context.WithCancel(t.Context())
			defer cancel()
			time.AfterFunc(c.cancelAfter, cancel)
			started := time.Now()
			result, err := r.testTransferResult(ctx, c.stage, 5*time.Second)
			elapsed := time.Since(started)
			if outcome := log.details().Outcome; !errors.Is(err, context.Canceled) || elapsed > 1500*time.Millisecond ||
				outcome != OutcomeStopped {
				t.Fatalf("stage returned %v (%s) after %v, want a prompt stop", err, outcome, elapsed)
			}
			if c.name == "silent download" && result.TotalBytes != 0 {
				t.Errorf("TotalBytes = %d from a server that wrote nothing", result.TotalBytes)
			}
		})
	}
}

func TestUploadLaneDrainsABoundedResponse(t *testing.T) {
	t.Parallel()
	synctest.Test(t, func(t *testing.T) {
		var drained, requests int
		transport := roundTripFunc(func(req *http.Request) (*http.Response, error) {
			if requests++; requests > 1 {
				<-req.Context().Done()
				return nil, req.Context().Err()
			}
			_, _ = io.CopyN(io.Discard, req.Body, 64*1024)
			body := io.NopCloser(readerFunc(func(p []byte) (int, error) {
				if drained >= 1<<20 {
					return 0, io.EOF
				}
				drained += len(p)
				return len(p), nil
			}))
			return &http.Response{StatusCode: http.StatusOK, Body: body, Request: req}, nil
		})
		r := &runner{http: &http.Client{Transport: transport}, target: fetchTarget("http://meter.test")}
		ctx, cancel := context.WithTimeout(t.Context(), time.Second)
		defer cancel()
		_ = lane(r, Up, "")(ctx)
		if drained > 2*maxControlBytes {
			t.Fatalf("an upload response was drained for %d bytes", drained)
		}
	})
}

type readerFunc func([]byte) (int, error)

func (f readerFunc) Read(p []byte) (int, error) { return f(p) }
