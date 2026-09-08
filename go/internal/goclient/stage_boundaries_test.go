package goclient

import (
	"context"
	"encoding/json/v2"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/http/httptest"
	"slices"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"testing/synctest"
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
	mux.HandleFunc("/upload/checkpoint", func(w http.ResponseWriter, _ *http.Request) {
		_ = json.MarshalWrite(w, struct {
			Bytes uint64 `json:"bytes"`
			Nanos uint64 `json:"nanos"`
		}{received.Load(), uint64(time.Since(started))})
	})
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
				result <- r.runTestStage(ctx, "bidirectional", 150*time.Millisecond)
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
			if len(phases) != 4 || phases[1].Phase != StageWarmup || phases[2].Phase != StageMeasuring || phases[3].Phase != StageFinished {
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
			var measuredAt time.Time
			var details *RunDetails
			r := &runner{cfg: cfg, streams: streamCounts{down: 1, up: 1}, http: srv.Client(), emit: func(e Event) {
				mu.Lock()
				defer mu.Unlock()
				if e.Kind == EventStage && e.Phase == StageMeasuring {
					measuredAt = e.At
				}
				if e.Kind == EventServers {
					details = e.Servers
				}
				if e.Kind == EventResult {
					results = append(results, *e.Result)
				}
				if e.Kind == EventThroughput && !e.Throughput.Unavailable && e.At.Sub(measuredAt) >= time.Second {
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
			err := r.runTestStage(ctx, "bidirectional", 3*time.Second)
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
			if time.Since(started) > 2*time.Second {
				t.Fatal("stage failure did not promptly cancel its siblings and release progress")
			}
			mu.Lock()
			defer mu.Unlock()
			if len(results) != 3 || results[0].Latency.Count == 0 {
				t.Fatalf("partial results=%+v", results)
			}
			if details == nil || details.Outcome != "incomplete" || len(details.Servers) != 1 || len(details.Servers[0].Results) != 3 {
				t.Fatalf("interruption lost the terminal summary: %+v", details)
			}
			for _, result := range results {
				if result.Err == nil {
					t.Fatalf("partial population lost its cause: %+v", result)
				}
				if result.Direction == "" {
					if result.Elapsed <= 0 || result.Elapsed >= 3*time.Second {
						t.Fatalf("latency window: %+v", result)
					}
					continue
				}
				if result.TotalBytes == 0 || result.ServerAuth != (result.Direction == Up) {
					t.Fatalf("missing receiver attribution: %+v", result)
				}
				if interruption == "cancel" {
					if result.Unavailable || result.MeanBps <= 0 {
						t.Fatalf("cancel discarded the measured interval: %+v", result)
					}
				} else if !result.Unavailable {
					t.Fatalf("removed participant retained a headline: %+v", result)
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
	err := r.runTestStage(ctx, "upload", time.Second)
	if _, ok := errors.AsType[*AuthRequiredError](err); !ok {
		t.Fatalf("upload progress cause=%v", err)
	}
	if measuring.Load() || time.Since(started) > time.Second {
		t.Fatalf("lost progress continued the warmup: measured=%v elapsed=%v", measuring.Load(), time.Since(started))
	}
}

func TestUploadSilentFeedAfterWarmupHasBoundedCheckpoint(t *testing.T) {
	warmup := make(chan struct{})
	checkpointStarted, checkpointStopped := make(chan struct{}), make(chan struct{})
	mux := http.NewServeMux()
	mux.HandleFunc("/upload/session", func(w http.ResponseWriter, _ *http.Request) { _, _ = fmt.Fprintln(w, `{"uploadId":"silent-feed"}`) })
	mux.HandleFunc("/upload", func(w http.ResponseWriter, r *http.Request) { _, _ = io.Copy(io.Discard, r.Body) })
	mux.HandleFunc("/upload/checkpoint", func(w http.ResponseWriter, r *http.Request) {
		close(checkpointStarted)
		defer close(checkpointStopped)
		<-r.Context().Done()
	})
	mux.HandleFunc("/upload/progress", func(w http.ResponseWriter, r *http.Request) {
		if r.Method == http.MethodDelete {
			return
		}
		_, _ = fmt.Fprintln(w, `{"type":"ready"}`)
		w.(http.Flusher).Flush()
		ticker := time.Tick(5 * time.Millisecond)
		for {
			select {
			case <-r.Context().Done():
				return
			case <-warmup:
				// Live progress cannot replace the bounded authoritative checkpoint.
				<-r.Context().Done()
				return
			case <-ticker:
				_, _ = fmt.Fprintln(w, `{"type":"progress","bytes":100,"nanos":10000000}`)
				w.(http.Flusher).Flush()
			}
		}
	})
	srv := httptest.NewServer(mux)
	defer srv.Close()
	var measured bool
	var details *RunDetails
	cfg := Config{BaseURL: srv.URL, Warmup: 20 * time.Millisecond, UploadBytesPerStream: 32 * 1024}.normalized()
	r := &runner{cfg: cfg, streams: streamCounts{up: 1}, http: srv.Client(), emit: func(e Event) {
		if e.Kind == EventStage && e.Phase == StageWarmup {
			close(warmup)
		}
		if e.Kind == EventResult || e.Kind == EventStage && e.Phase == StageMeasuring {
			measured = true
		}
		if e.Kind == EventServers {
			details = e.Servers
		}
	}}
	ctx, cancel := context.WithTimeout(t.Context(), 4*time.Second)
	defer cancel()
	started := time.Now()
	err := r.runTestStage(ctx, "upload", 50*time.Millisecond)
	if err == nil || !strings.Contains(err.Error(), "receiver checkpoint unavailable before measurement") {
		t.Fatalf("silent checkpoint cause = %v", err)
	}
	if measured || details == nil || details.Outcome != "incomplete" || len(details.Intervals) != 0 {
		t.Fatalf("preparation became measurement: measured=%v details=%+v", measured, details)
	}
	select {
	case <-checkpointStarted:
	default:
		t.Fatal("checkpoint never requested")
	}
	select {
	case <-checkpointStopped:
	case <-time.After(time.Second):
		t.Fatal("checkpoint request not canceled")
	}
	if elapsed := time.Since(started); elapsed < 1500*time.Millisecond || elapsed > 3*time.Second {
		t.Fatalf("checkpoint collection exceeded its independent budget: %v", elapsed)
	}
}

func TestTransferZeroProgressUsesEvidenceAndLivenessRules(t *testing.T) {
	for _, stage := range []string{"download", "upload"} {
		for _, duration := range []time.Duration{100 * time.Millisecond, time.Second, 3 * time.Second} {
			t.Run(stage+"/"+duration.String(), func(t *testing.T) {
				var srv *httptest.Server
				if stage == "download" {
					srv = newSilentDownloadServer()
				} else {
					srv = newStalledUploadServer()
				}
				defer srv.Close()
				var details *RunDetails
				r := &runner{
					cfg:     Config{BaseURL: srv.URL, UploadBytesPerStream: 32 * 1024}.normalized(),
					streams: streamCounts{down: 1, up: 1},
					http:    srv.Client(),
					emit: func(e Event) {
						if e.Kind == EventServers {
							details = e.Servers
						}
					},
				}
				ctx, cancel := context.WithTimeout(t.Context(), 5*time.Second)
				defer cancel()
				started := time.Now()
				result, err := r.testTransferResult(ctx, stage, duration)
				if duration > busRedialWindow {
					if !errors.Is(err, errNoSurvivors) || details == nil || len(details.Failures) != 1 || !strings.Contains(details.Failures[0].Message, "stopped delivering bytes") {
						t.Fatalf("stalled participant survived: err=%v details=%+v", err, details)
					}
					if time.Since(started) >= duration {
						t.Fatal("liveness failure waited for the stage deadline")
					}
				} else if err != nil {
					t.Fatal(err)
				}
				if result.TotalBytes != 0 || result.MeanBps != 0 || result.ServerAuth != (stage == "upload") {
					t.Fatalf("zero progress invented data: %+v", result)
				}
				wantUnavailable := duration < minimumSurvivorEvidence || duration > busRedialWindow
				if result.Unavailable != wantUnavailable {
					t.Fatalf("evidence availability: %+v", result)
				}
			})
		}
	}
}

func TestCoordinatorSetupTimeoutAndCancellationCannotMeasure(t *testing.T) {
	for _, cancelEarly := range []bool{false, true} {
		t.Run(fmt.Sprint(cancelEarly), func(t *testing.T) {
			synctest.Test(t, func(t *testing.T) {
				var phases []StagePhase
				var details *RunDetails
				var active atomic.Int64
				r := &runner{
					cfg:     Config{BaseURL: "http://fixture.invalid", DownloadBytesPerStream: 1024}.normalized(),
					streams: streamCounts{down: 1},
					http: &http.Client{Transport: roundTripFunc(func(req *http.Request) (*http.Response, error) {
						active.Add(1)
						defer func() { active.Add(-1) }()
						<-req.Context().Done()
						return nil, req.Context().Err()
					})},
					emit: func(e Event) {
						if e.Kind == EventStage {
							phases = append(phases, e.Phase)
						}
						if e.Kind == EventServers {
							details = e.Servers
						}
						if e.Kind == EventResult || e.Kind == EventThroughput {
							t.Errorf("unready stage emitted data: %+v", e)
						}
					},
				}
				ctx, cancel := context.WithCancel(t.Context())
				defer cancel()
				done := make(chan error, 1)
				go func() { done <- r.runTestStage(ctx, "download", time.Second) }()
				synctest.Wait()
				if active.Load() != 1 {
					t.Fatalf("fixture did not own a preparing request: %d", active.Load())
				}
				if cancelEarly {
					cancel()
				} else {
					time.Sleep(stageReadyTimeout)
				}
				err := <-done
				if err == nil || cancelEarly && !errors.Is(err, context.Canceled) {
					t.Fatalf("setup cause=%v", err)
				}
				if active.Load() != 0 || !slices.Equal(phases, []StagePhase{StagePreparing}) || details == nil || details.Outcome != "incomplete" {
					t.Fatalf("setup cleanup: active=%d phases=%v details=%+v", active.Load(), phases, details)
				}
			})
		})
	}
}

func TestCoordinatorExcludesPreparationBytesAndTime(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		const preparationBytes = 64 * 1024
		// Keep fake-time completion between ticks so separate captures advance time.
		const measuredWindow = 1100 * time.Millisecond
		var requests int
		var preparedAt, measuredAt time.Time
		var details *RunDetails
		r := &runner{
			cfg:     Config{BaseURL: "http://fixture.invalid", Warmup: 100 * time.Millisecond, DownloadBytesPerStream: preparationBytes}.normalized(),
			streams: streamCounts{down: 1},
			http: &http.Client{Transport: roundTripFunc(func(req *http.Request) (*http.Response, error) {
				requests++
				if requests == 1 {
					return &http.Response{StatusCode: http.StatusOK, Body: io.NopCloser(strings.NewReader(strings.Repeat("x", preparationBytes))), Request: req}, nil
				}
				<-req.Context().Done()
				return nil, req.Context().Err()
			})},
			emit: func(e Event) {
				if e.Kind == EventServers {
					details = e.Servers
				}
				if e.Kind == EventStage && e.Phase == StagePreparing {
					preparedAt = e.At
				}
				if e.Kind == EventStage && e.Phase == StageMeasuring {
					measuredAt = e.At
				}
			},
		}
		result, err := r.testTransferResult(t.Context(), "download", measuredWindow)
		if err != nil {
			t.Fatal(err)
		}
		if got := r.coordinated.download(); got != preparationBytes {
			t.Fatalf("preparation fixture carried %d bytes", got)
		}
		if measuredAt.Sub(preparedAt) != r.cfg.Warmup || result.TotalBytes != 0 || result.Elapsed != measuredWindow || result.Unavailable {
			t.Fatalf("preparation contaminated the measured window: preparation=%v result=%+v intervals=%+v", measuredAt.Sub(preparedAt), result, details.Intervals)
		}
	})
}
