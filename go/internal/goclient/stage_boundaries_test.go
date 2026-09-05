package goclient

import (
	"context"
	"encoding/json/v2"
	"errors"
	"fmt"
	"io"
	"math"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"time"
)

// The fixture preserves the upload receiver's counters and closes its feed on
// DELETE so teardown timing cannot hide a delayed sibling cancellation.
func mountStageUpload(mux *http.ServeMux, received *atomic.Uint64, upload http.HandlerFunc) {
	mux.HandleFunc("/upload/session", func(w http.ResponseWriter, _ *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		_ = json.MarshalWrite(w, uploadSessionResponse{UploadID: "stage-boundary"})
	})
	mux.HandleFunc("/upload", upload)
	done := make(chan struct{})
	var once sync.Once
	started := time.Now()
	mux.HandleFunc("/upload/progress", func(w http.ResponseWriter, r *http.Request) {
		if r.Method == http.MethodDelete {
			once.Do(func() { close(done) })
			w.WriteHeader(http.StatusNoContent)
			return
		}
		w.Header().Set("Content-Type", "application/x-ndjson")
		flush := w.(http.Flusher)
		send := func(kind string) {
			_, _ = fmt.Fprintf(w, "{\"type\":%q,\"bytes\":%d,\"nanos\":%d}\n", kind, received.Load(), time.Since(started))
			flush.Flush()
		}
		send("ready")
		ticker := time.Tick(10 * time.Millisecond)
		for {
			select {
			case <-r.Context().Done():
				return
			case <-done:
				send("complete")
				return
			case <-ticker:
				send("progress")
			}
		}
	})
}

func TestTransferWarmupWaitsForDelayedTransports(t *testing.T) {
	for _, delayed := range []string{"download lane", "upload session", "upload progress", "latency bus"} {
		t.Run(delayed, func(t *testing.T) {
			held, release := make(chan struct{}), make(chan struct{})
			var holdOnce sync.Once
			var uploaded atomic.Uint64
			mux := http.NewServeMux()
			mux.HandleFunc("/download", func(w http.ResponseWriter, _ *http.Request) { _, _ = w.Write(make([]byte, 32*1024)) })
			mux.Handle("/ws/ping", echoPingHandler())
			mountStageUpload(mux, &uploaded, func(w http.ResponseWriter, r *http.Request) {
				n, _ := io.Copy(io.Discard, r.Body)
				uploaded.Add(uint64(n))
			})
			srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				delay := delayed == "download lane" && r.URL.Path == "/download" && r.URL.Query().Get("lane") == "1" ||
					delayed == "upload session" && r.URL.Path == "/upload/session" ||
					delayed == "upload progress" && r.URL.Path == "/upload/progress" && r.Method == http.MethodGet ||
					delayed == "latency bus" && r.URL.Path == "/ws/ping"
				if delay {
					holdOnce.Do(func() { close(held) })
					select {
					case <-release:
					case <-r.Context().Done():
						return
					}
				}
				mux.ServeHTTP(w, r)
			}))
			defer srv.Close()
			cfg := Config{BaseURL: srv.URL, Warmup: 80 * time.Millisecond, LoadedLatency: true, PingInterval: 10 * time.Millisecond, DownloadBytesPerStream: 32 * 1024, UploadBytesPerStream: 32 * 1024}.normalized()
			var mu sync.Mutex
			var phases []Event
			var samples int
			r := &runner{cfg: cfg, streams: streamCounts{down: 2, up: 2}, http: srv.Client(), emit: func(e Event) {
				mu.Lock()
				defer mu.Unlock()
				if e.Kind == EventStage {
					phases = append(phases, e)
				}
				if e.Kind == EventLatency || e.Kind == EventThroughput {
					samples++
				}
			}}
			attachTestLatencyTarget(r, srv.URL)
			ctx, cancel := context.WithTimeout(t.Context(), 5*time.Second)
			defer cancel()
			result := make(chan error, 1)
			go func() {
				result <- r.runTransferStage(ctx, "bidirectional", []Direction{Down, Up}, 150*time.Millisecond)
			}()
			select {
			case <-held:
			case <-ctx.Done():
				t.Fatal("delayed transport was never opened")
			}
			time.Sleep(120 * time.Millisecond) // Longer than warmup: setup must not consume it.
			mu.Lock()
			if len(phases) != 1 || phases[0].Phase != StagePreparing || samples != 0 {
				t.Errorf("before transport ready: phases=%v samples=%d", phases, samples)
			}
			mu.Unlock()
			releasedAt := time.Now()
			close(release)
			if err := <-result; err != nil {
				t.Fatal(err)
			}
			mu.Lock()
			defer mu.Unlock()
			if len(phases) != 3 || phases[1].Phase != StageWarmup || phases[2].Phase != StageMeasuring {
				t.Fatalf("phases=%v", phases)
			}
			if phases[1].At.Before(releasedAt) || phases[2].At.Sub(phases[1].At) < cfg.Warmup {
				t.Fatalf("warmup did not follow readiness: %v", phases)
			}
			if samples == 0 {
				t.Fatal("ready transports never measured")
			}
		})
	}
}

func TestInterruptedTransferPreservesAttributableReceiverWindows(t *testing.T) {
	for _, interruption := range []string{"download failure", "upload failure", "cancel"} {
		t.Run(interruption, func(t *testing.T) {
			ctx, cancel := context.WithTimeout(t.Context(), 5*time.Second)
			defer cancel()
			var uploaded atomic.Uint64
			var canInterrupt atomic.Bool
			mux := http.NewServeMux()
			mux.Handle("/ws/ping", echoPingHandler())
			mux.HandleFunc("/download", func(w http.ResponseWriter, _ *http.Request) {
				if canInterrupt.Load() && interruption == "download failure" {
					w.WriteHeader(http.StatusServiceUnavailable)
					return
				}
				time.Sleep(time.Millisecond)
				_, _ = w.Write(make([]byte, 32*1024))
			})
			mountStageUpload(mux, &uploaded, func(w http.ResponseWriter, r *http.Request) {
				if canInterrupt.Load() && interruption == "upload failure" {
					w.WriteHeader(http.StatusServiceUnavailable)
					return
				}
				n, _ := io.Copy(io.Discard, r.Body)
				uploaded.Add(uint64(n))
				time.Sleep(time.Millisecond)
			})
			srv := httptest.NewServer(mux)
			defer srv.Close()
			cfg := Config{BaseURL: srv.URL, LoadedLatency: true, PingInterval: 10 * time.Millisecond, DownloadBytesPerStream: 32 * 1024, UploadBytesPerStream: 32 * 1024}.normalized()
			var mu sync.Mutex
			seen := map[Direction]bool{}
			var results []Result
			r := &runner{cfg: cfg, streams: streamCounts{down: 1, up: 1}, http: srv.Client(), emit: func(e Event) {
				mu.Lock()
				defer mu.Unlock()
				if e.Kind == EventResult {
					results = append(results, *e.Result)
				}
				if e.Kind == EventThroughput {
					seen[e.Direction] = true
					if seen[Down] && seen[Up] {
						canInterrupt.Store(true)
						if interruption == "cancel" {
							cancel()
						}
					}
				}
			}}
			attachTestLatencyTarget(r, srv.URL)
			started := time.Now()
			err := r.runTransferStage(ctx, "bidirectional", []Direction{Down, Up}, 3*time.Second)
			if err == nil {
				t.Fatal("interrupted stage reported success")
			}
			if interruption == "cancel" {
				if !errors.Is(err, context.Canceled) {
					t.Fatal(err)
				}
			} else if !strings.Contains(err.Error(), "503") {
				t.Fatal(err)
			}
			if time.Since(started) > time.Second {
				t.Fatal("stage failure did not promptly cancel its siblings and release progress")
			}
			mu.Lock()
			defer mu.Unlock()
			if len(results) != 3 || results[0].Latency.Count == 0 {
				t.Fatalf("partial results=%+v", results)
			}
			for _, result := range results {
				if !errors.Is(result.Err, err) || result.Elapsed <= 0 || result.Elapsed >= 3*time.Second {
					t.Fatalf("partial population lost its cause/window: %+v", result)
				}
				if result.Direction == "" {
					continue
				}
				if result.TotalBytes == 0 || result.MeanBps <= 0 || result.ServerAuth != (result.Direction == Up) {
					t.Fatalf("missing receiver attribution: %+v", result)
				}
				if math.Abs(result.MeanBps-float64(result.TotalBytes)/result.Elapsed.Seconds()) > 1e-9 {
					t.Fatalf("rate not derived from receiver window: %+v", result)
				}
			}
		})
	}
}

func TestUploadProgressFailureCancelsTheStageBeforeWarmupEnds(t *testing.T) {
	var rejectProgress atomic.Bool
	var uploaded atomic.Uint64
	mux := http.NewServeMux()
	mountStageUpload(mux, &uploaded, func(_ http.ResponseWriter, r *http.Request) {
		n, _ := io.Copy(io.Discard, r.Body)
		uploaded.Add(uint64(n))
	})
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path == "/upload/progress" && r.Method == http.MethodGet {
			if rejectProgress.Load() {
				w.Header().Set("Graphite-Meter-Auth", "required")
				w.WriteHeader(http.StatusForbidden)
				return
			}
			w.Header().Set("Content-Type", "application/x-ndjson")
			_, _ = fmt.Fprintln(w, "{\"type\":\"ready\"}")
			w.(http.Flusher).Flush()
			for !rejectProgress.Load() {
				select {
				case <-r.Context().Done():
					return
				case <-time.After(10 * time.Millisecond):
				}
				_, _ = fmt.Fprintf(w, "{\"type\":\"progress\",\"bytes\":%d,\"nanos\":1}\n", uploaded.Load())
				w.(http.Flusher).Flush()
			}
			return
		}
		mux.ServeHTTP(w, r)
	}))
	defer srv.Close()
	cfg := Config{BaseURL: srv.URL, Warmup: 4 * time.Second, UploadBytesPerStream: 32 * 1024}.normalized()
	var measuring atomic.Bool
	r := &runner{cfg: cfg, streams: streamCounts{up: 1}, http: srv.Client(), emit: func(e Event) {
		if e.Kind == EventStage && e.Phase == StageWarmup {
			rejectProgress.Store(true)
		}
		if e.Kind == EventStage && e.Phase == StageMeasuring || e.Kind == EventResult {
			measuring.Store(true)
		}
	}}
	ctx, cancel := context.WithTimeout(t.Context(), 5*time.Second)
	defer cancel()
	started := time.Now()
	err := r.runTransferStage(ctx, "upload", []Direction{Up}, time.Second)
	if _, ok := errors.AsType[*AuthRequiredError](err); !ok {
		t.Fatalf("upload progress cause=%v", err)
	}
	if measuring.Load() || time.Since(started) > time.Second {
		t.Fatalf("lost progress continued the warmup: measured=%v elapsed=%v", measuring.Load(), time.Since(started))
	}
}
