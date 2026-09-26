package goclient

import (
	"bytes"
	"context"
	"errors"
	"io"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync/atomic"
	"testing"
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

func TestLanesRetryABusyServerAndStopOnARefusal(t *testing.T) {
	t.Parallel()
	for _, c := range []struct {
		status  int
		want    FailureReason
		retried bool
	}{{http.StatusTooManyRequests, FailureServerBusy, true}, {http.StatusServiceUnavailable, FailureServerBusy, true},
		{http.StatusGone, FailureProtocol, false}} {
		for _, dir := range []Direction{Down, Up} {
			synctest.Test(t, func(t *testing.T) {
				requests := 0
				transport := roundTripFunc(func(req *http.Request) (*http.Response, error) {
					requests++
					return &http.Response{StatusCode: c.status, Body: http.NoBody, Request: req}, nil
				})
				r := &runner{http: &http.Client{Transport: transport}, target: fetchTarget("http://meter.test")}
				err := lane(r, dir, "http://meter.test/download")(t.Context())
				if failureReason(err, false) != c.want || (requests > 1) != c.retried {
					t.Errorf("%s %d: %v after %d requests", dir, c.status, err, requests)
				}
			})
		}
	}
}

func TestLanesThatMoveNothingEndWithTheLastError(t *testing.T) {
	t.Parallel()
	refused := errors.New("connection refused")
	for _, c := range []struct {
		name string
		dir  Direction
		fail error
		want string
	}{
		{"download refused", Down, refused, "connection refused"},
		{"download empty", Down, nil, errNoBytes.Error()},
		{"upload refused", Up, refused, "connection refused"},
	} {
		t.Run(c.name, func(t *testing.T) {
			synctest.Test(t, func(t *testing.T) {
				requests := 0
				transport := roundTripFunc(func(req *http.Request) (*http.Response, error) {
					requests++
					if c.fail != nil {
						return nil, c.fail
					}
					return &http.Response{StatusCode: http.StatusOK, Body: http.NoBody, Request: req}, nil
				})
				r := &runner{http: &http.Client{Transport: transport}, target: fetchTarget("http://meter.test")}
				err := lane(r, c.dir, "http://meter.test/download")(t.Context())
				if err == nil || !strings.Contains(err.Error(), c.want) {
					t.Fatalf("lane ended with %v, want %q", err, c.want)
				}
				if paced := int(redialWindow/retryBackoff) + 1; requests > paced {
					t.Errorf("%d requests before giving up, want at most %d", requests, paced)
				}
			})
		})
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

func TestAnIdleUploadLaneRedialsInsteadOfFailing(t *testing.T) {
	t.Parallel()
	for _, refusal := range []string{"idle", ""} {
		synctest.Test(t, func(t *testing.T) {
			requests := 0
			transport := roundTripFunc(func(req *http.Request) (*http.Response, error) {
				if requests++; requests > 1 {
					<-req.Context().Done()
					return nil, req.Context().Err()
				}
				_, _ = io.CopyN(io.Discard, req.Body, 1024)
				header := http.Header{"X-Graphite-Upload-Refusal": {refusal}}
				return &http.Response{StatusCode: http.StatusRequestTimeout, Header: header, Body: http.NoBody,
					Request: req}, nil
			})
			r := &runner{http: &http.Client{Transport: transport}, target: fetchTarget("http://meter.test")}
			ctx, cancel := context.WithTimeout(t.Context(), time.Second)
			defer cancel()
			if err := lane(r, Up, "")(ctx); (err == nil) != (refusal == "idle") || (requests > 1) != (err == nil) {
				t.Errorf("408 %q: lane ended with %v after %d requests", refusal, err, requests)
			}
		})
	}
}

type readerFunc func([]byte) (int, error)

func (f readerFunc) Read(p []byte) (int, error) { return f(p) }
