package goclient

import (
	"context"
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

func TestTransferWarmupWaitsForDelayedTransports(t *testing.T) {
	t.Parallel()
	for _, delayed := range []string{"download lane", "upload session", "upload progress", "latency bus"} {
		t.Run(delayed, func(t *testing.T) {
			t.Parallel()
			synctest.Test(t, func(t *testing.T) {
				held, release := make(chan struct{}), make(chan struct{})
				var holdOnce sync.Once
				mux := http.NewServeMux()
				mux.HandleFunc("/download", func(w http.ResponseWriter, _ *http.Request) {
					time.Sleep(time.Millisecond)
					_, _ = w.Write(make([]byte, 32*1024))
				})
				mux.Handle("/ws/ping", pingHandler(answerAll, 0))
				mountUploadReceiver(mux, nil)
				r := pipedRunner(t, http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
					path := r.URL.Path
					delay := delayed == "download lane" && path == "/download" && r.URL.Query().Get("lane") == "1" ||
						delayed == "upload session" && path == "/upload/session" ||
						delayed == "upload progress" && path == "/upload/progress" && r.Method == http.MethodGet ||
						delayed == "latency bus" && path == "/ws/ping"
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
				r.cfg.Warmup, r.cfg.LoadedLatency = 80*time.Millisecond, true
				r.cfg.PingInterval, r.cfg.LoadedPingInterval = 10*time.Millisecond, 10*time.Millisecond
				r.streams = byDirection[int]{down: 2, up: 2}
				var log eventLog
				r.emit = log.emit
				result := make(chan error, 1)
				go func() { result <- r.runTestStage(t.Context(), StageBidirectional, 150*time.Millisecond) }()
				<-held
				samples := func() (n int) {
					for _, e := range log.all() {
						if e.Kind == EventLatency || e.Kind == EventThroughput {
							n++
						}
					}
					return n
				}
				time.Sleep(120 * time.Millisecond) // Longer than warmup: setup must not consume it.
				if phases := log.phases(); !slices.Equal(phases, []Phase{PhasePreparing}) || samples() != 0 {
					t.Errorf("before transport ready: phases=%v samples=%d", phases, samples())
				}
				releasedAt := time.Now()
				close(release)
				if err := <-result; err != nil {
					t.Fatal(err)
				}
				phases := slices.DeleteFunc(log.all(), func(e Event) bool { return e.Kind != EventStage })
				if len(phases) != 4 || phases[1].Phase != PhaseWarmup || phases[2].Phase != PhaseMeasuring ||
					phases[3].Phase != PhaseFinished || samples() == 0 {
					t.Fatalf("phases=%v samples=%d", phases, samples())
				}
				if phases[1].At.Before(releasedAt) || phases[2].At.Sub(phases[1].At) < r.cfg.Warmup {
					t.Fatalf("warmup did not follow readiness: %v", phases)
				}
			})
		})
	}
}

func TestInterruptedTransferPreservesAttributableReceiverWindows(t *testing.T) {
	t.Parallel()
	for _, interruption := range []string{"download failure", "upload failure", "cancel"} {
		t.Run(interruption, func(t *testing.T) {
			t.Parallel()
			synctest.Test(t, func(t *testing.T) {
				ctx, cancel := context.WithCancel(t.Context())
				defer cancel()
				var canInterrupt atomic.Bool
				mux := http.NewServeMux()
				mux.Handle("/ws/ping", pingHandler(answerAll, 0))
				mux.HandleFunc("/download", func(w http.ResponseWriter, _ *http.Request) {
					if canInterrupt.Load() && interruption == "download failure" {
						w.WriteHeader(http.StatusGone)
						return
					}
					time.Sleep(time.Millisecond)
					_, _ = w.Write(make([]byte, 32*1024))
				})
				uploadFails := func() bool { return canInterrupt.Load() && interruption == "upload failure" }
				mountUploadReceiver(mux, uploadFails)
				r := pipedRunner(t, mux)
				r.cfg.LoadedLatency, r.cfg.PingInterval, r.cfg.LoadedPingInterval = true, 10*time.Millisecond,
					10*time.Millisecond
				var log eventLog
				var measuredAt time.Time
				seen := map[Direction]bool{}
				r.emit = func(e Event) {
					log.emit(e)
					if e.Kind == EventStage && e.Phase == PhaseMeasuring {
						measuredAt = e.At
					}
					if e.Kind == EventThroughput && !e.Throughput.Unavailable && e.At.Sub(measuredAt) >= time.Second {
						if seen[e.Direction] = true; seen[Down] && seen[Up] {
							canInterrupt.Store(true)
							if interruption == "cancel" {
								cancel()
							}
						}
					}
				}
				started := time.Now()
				err := r.runTestStage(ctx, StageBidirectional, 3*time.Second)
				results, details := log.results(), log.details()
				failure := err
				if interruption != "cancel" && err == nil && len(results) > 0 {
					failure = results[0].Err
				}
				failed := interruption == "cancel" && errors.Is(err, context.Canceled) ||
					err == nil && failure != nil && strings.Contains(failure.Error(), "410")
				if !failed {
					t.Fatalf("interrupted stage = %v, %v", err, failure)
				}
				if time.Since(started) > 2*time.Second {
					t.Fatal("stage failure did not promptly cancel its siblings and release progress")
				}
				want := OutcomeIncomplete
				if interruption == "cancel" {
					want = OutcomeStopped
				}
				if len(results) != 2 || details == nil || details.Outcome != want || len(details.Servers) != 1 ||
					len(details.Servers[0].Results) != 3 {
					t.Fatalf("interruption lost the terminal summary: results=%+v details=%+v", results, details)
				}
				if latency := details.Servers[0].Results[0]; latency.Direction != "" || latency.Latency.Count == 0 ||
					latency.Err == nil || latency.Elapsed <= 0 || latency.Elapsed >= 3*time.Second {
					t.Fatalf("latency population: %+v", latency)
				}
				for _, result := range results {
					if result.Err == nil || result.TotalBytes == 0 {
						t.Fatalf("partial population lost its cause or receiver attribution: %+v", result)
					}
					if interruption == "cancel" && (result.Unavailable || result.MeanBps <= 0) {
						t.Fatalf("cancel discarded the measured interval: %+v", result)
					}
					if interruption != "cancel" && !result.Unavailable {
						t.Fatalf("removed participant retained a headline: %+v", result)
					}
				}
			})
		})
	}
}

func TestUploadReceiverEvidenceFailsBeforeMeasuring(t *testing.T) {
	t.Parallel()
	for _, c := range []struct {
		name    string
		revoked bool
		warmup  time.Duration
		elapsed time.Duration
	}{
		{"revoked progress feed", true, 4 * time.Second, 510 * time.Millisecond},
		{"silent progress feed", false, 20 * time.Millisecond, 1530 * time.Millisecond},
	} {
		t.Run(c.name, func(t *testing.T) {
			t.Parallel()
			synctest.Test(t, func(t *testing.T) {
				warm := make(chan struct{})
				var checkpointStarted, checkpointStopped atomic.Bool
				mux := http.NewServeMux()
				mountUploadReceiver(mux, nil)
				r := pipedRunner(t, http.HandlerFunc(func(w http.ResponseWriter, req *http.Request) {
					switch {
					case req.URL.Path == "/upload/checkpoint" && !c.revoked:
						checkpointStarted.Store(true)
						<-req.Context().Done()
						checkpointStopped.Store(true)
					case req.URL.Path == "/upload/progress" && req.Method == http.MethodGet:
						serveProgressUntilWarm(w, req, warm, c.revoked)
					default:
						mux.ServeHTTP(w, req)
					}
				}))
				r.cfg.Warmup, r.streams = c.warmup, byDirection[int]{up: 1}
				var log eventLog
				var once sync.Once
				r.emit = func(e Event) {
					if e.Kind == EventStage && e.Phase == PhaseWarmup {
						once.Do(func() { close(warm) })
					}
					log.emit(e)
				}
				started := time.Now()
				err := r.runTestStage(t.Context(), StageUpload, time.Second)
				elapsed := time.Since(started)
				synctest.Wait()
				_, auth := errors.AsType[*AuthRequiredError](err)
				if err == nil || auth != c.revoked ||
					!c.revoked && !strings.Contains(err.Error(), "receiver checkpoint unavailable before measurement") {
					t.Fatalf("receiver evidence failure = %v", err)
				}
				details := log.details()
				if slices.Contains(log.phases(), PhaseMeasuring) || len(log.results()) != 0 || details == nil ||
					details.Outcome != OutcomeFailed || len(details.Intervals) != 0 {
					t.Fatalf("preparation became measurement: %+v %+v", log.phases(), details)
				}
				if checkpointStarted.Load() != !c.revoked || checkpointStopped.Load() != !c.revoked {
					t.Fatalf("checkpoint started=%v stopped=%v", checkpointStarted.Load(), checkpointStopped.Load())
				}
				if elapsed != c.elapsed {
					t.Fatalf("receiver evidence failed after %v, want %v", elapsed, c.elapsed)
				}
			})
		})
	}
}

func serveProgressUntilWarm(w http.ResponseWriter, r *http.Request, warm <-chan struct{}, revoke bool) {
	select {
	case <-warm:
		if revoke {
			w.Header().Set("Graphite-Meter-Auth", "required")
			w.WriteHeader(http.StatusForbidden)
			return
		}
	default:
	}
	_, _ = fmt.Fprintln(w, `{"type":"ready"}`)
	w.(http.Flusher).Flush()
	for nanos := 1; ; nanos++ {
		select {
		case <-r.Context().Done():
			return
		case <-warm:
			if !revoke {
				<-r.Context().Done()
			}
			return
		case <-time.After(10 * time.Millisecond):
		}
		_, _ = fmt.Fprintf(w, "{\"type\":\"progress\",\"bytes\":%d,\"nanos\":%d}\n", nanos, nanos)
		w.(http.Flusher).Flush()
	}
}

func TestTransferZeroProgressUsesEvidenceAndLivenessRules(t *testing.T) {
	t.Parallel()
	for _, stage := range []Stage{StageDownload, StageUpload} {
		for _, duration := range []time.Duration{100 * time.Millisecond, 3 * time.Second} {
			t.Run(string(stage)+"/"+duration.String(), func(t *testing.T) {
				t.Parallel()
				var srv *httptest.Server
				if stage == StageDownload {
					srv = httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
						w.WriteHeader(http.StatusOK)
						w.(http.Flusher).Flush()
						<-r.Context().Done()
					}))
				} else {
					mux := http.NewServeMux()
					mountSilentReceiver(mux)
					srv = httptest.NewServer(mux)
				}
				defer srv.Close()
				var log eventLog
				r := &runner{
					cfg:     Config{BaseURL: srv.URL}.normalized(),
					streams: byDirection[int]{down: 1, up: 1},
					http:    srv.Client(),
					emit:    log.emit,
				}
				ctx, cancel := context.WithTimeout(t.Context(), 5*time.Second)
				defer cancel()
				started := time.Now()
				result, err := r.testTransferResult(ctx, stage, duration)
				details := log.details()
				if duration > redialWindow {
					if err != nil || !errors.Is(result.Err, errStageSkipped) ||
						details == nil ||
						len(details.Failures) != 1 ||
						!strings.Contains(details.Failures[0].Err.Error(), "stopped delivering bytes") {
						t.Fatalf("stalled participant survived: err=%v details=%+v", err, details)
					}
					if time.Since(started) >= duration {
						t.Fatal("liveness failure waited for the stage deadline")
					}
				} else if err != nil {
					t.Fatal(err)
				}
				if result.TotalBytes != 0 || result.MeanBps != 0 {
					t.Fatalf("zero progress invented data: %+v", result)
				}
				wantUnavailable := duration < minimumSurvivorEvidence || duration > redialWindow
				if result.Unavailable != wantUnavailable {
					t.Fatalf("evidence availability: %+v", result)
				}
			})
		}
	}
}

func TestCoordinatorSetupFailureCannotMeasure(t *testing.T) {
	t.Parallel()
	for _, c := range []struct {
		name, want string
		stage      Stage
		outcome    Outcome
	}{
		{"ready timeout", "not ready within", StageDownload, OutcomeFailed},
		{"cancelled", "context canceled", StageDownload, OutcomeStopped},
		{"sibling resource failure", "HTTP 500", StageBidirectional, OutcomeFailed},
	} {
		t.Run(c.name, func(t *testing.T) {
			t.Parallel()
			synctest.Test(t, func(t *testing.T) {
				requests := make(chan context.Context, 8)
				served := make(chan struct{})
				var serve sync.Once
				var log eventLog
				r := &runner{
					cfg:     Config{BaseURL: "http://fixture.invalid"}.normalized(),
					streams: byDirection[int]{down: 1, up: 1},
					http: &http.Client{Transport: roundTripFunc(func(req *http.Request) (*http.Response, error) {
						requests <- req.Context()
						switch {
						case c.stage == StageDownload:
							<-req.Context().Done()
							return nil, req.Context().Err()
						case req.URL.Path == "/upload/session":
							<-served
							return &http.Response{StatusCode: http.StatusInternalServerError, Body: http.NoBody,
								Request: req}, nil
						}
						body := readerFunc(func(p []byte) (int, error) {
							time.Sleep(time.Millisecond)
							serve.Do(func() { close(served) })
							return len(p), req.Context().Err()
						})
						return &http.Response{StatusCode: http.StatusOK, Body: io.NopCloser(body), Request: req}, nil
					})},
					emit: log.emit,
				}
				ctx, cancel := context.WithCancel(t.Context())
				defer cancel()
				done := make(chan error, 1)
				go func() { done <- r.runTestStage(ctx, c.stage, time.Second) }()
				synctest.Wait()
				if c.stage == StageDownload && len(requests) != 1 {
					t.Fatalf("fixture did not own a preparing request: %d", len(requests))
				}
				switch c.name {
				case "cancelled":
					cancel()
				case "ready timeout":
					time.Sleep(stageReadyTimeout)
				}
				err := <-done
				if err == nil || !strings.Contains(err.Error(), c.want) {
					t.Fatalf("setup cause=%v, want %q", err, c.want)
				}
				close(requests)
				for request := range requests {
					if request.Err() == nil {
						t.Fatal("a request outlived its failed stage")
					}
				}
				if details := log.details(); !slices.Equal(log.phases(), []Phase{PhasePreparing}) || details == nil ||
					details.Outcome != c.outcome {
					t.Fatalf("setup cleanup: phases=%v details=%+v", log.phases(), details)
				}
				for _, e := range log.all() {
					if e.Kind == EventResult || e.Kind == EventThroughput || e.Kind == EventLatency {
						t.Fatalf("unready stage emitted data: %+v", e)
					}
				}
			})
		})
	}
}

func TestCoordinatorExcludesPreparationBytesAndTime(t *testing.T) {
	t.Parallel()
	synctest.Test(t, func(t *testing.T) {
		const preparationBytes = 64 * 1024
		// Keep fake-time completion between ticks so separate captures advance time.
		const measuredWindow = 1100 * time.Millisecond
		var requests int
		var preparedAt, measuredAt time.Time
		var details *RunDetails
		r := &runner{
			cfg:     Config{BaseURL: "http://fixture.invalid", Warmup: 100 * time.Millisecond}.normalized(),
			streams: byDirection[int]{down: 1},
			http: &http.Client{Transport: roundTripFunc(func(req *http.Request) (*http.Response, error) {
				requests++
				if requests == 1 {
					return &http.Response{
						StatusCode: http.StatusOK,
						Body:       io.NopCloser(strings.NewReader(strings.Repeat("x", preparationBytes))),
						Request:    req,
					}, nil
				}
				<-req.Context().Done()
				return nil, req.Context().Err()
			})},
			emit: func(e Event) {
				if e.Servers != nil {
					details = e.Servers
				}
				if e.Kind == EventStage && e.Phase == PhasePreparing {
					preparedAt = e.At
				}
				if e.Kind == EventStage && e.Phase == PhaseMeasuring {
					measuredAt = e.At
				}
			},
		}
		result, err := r.testTransferResult(t.Context(), "download", measuredWindow)
		if err != nil {
			t.Fatal(err)
		}
		if got := r.coordinated.down.Load(); got != preparationBytes {
			t.Fatalf("preparation fixture carried %d bytes", got)
		}
		window := details.Intervals[0]
		if measuredAt.Sub(preparedAt) != r.cfg.Warmup ||
			result.TotalBytes != 0 ||
			window.End-window.Start != measuredWindow.Truncate(sampleInterval) ||
			!result.Unavailable {
			t.Fatalf(
				"preparation contaminated the measured window: preparation=%v result=%+v intervals=%+v",
				measuredAt.Sub(preparedAt),
				result,
				details.Intervals,
			)
		}
	})
}
