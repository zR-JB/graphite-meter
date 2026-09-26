package goclient

import (
	"errors"
	"fmt"
	"net/http"
	"net/http/httptest"
	"slices"
	"sync/atomic"
	"testing"
	"time"

	"github.com/coder/websocket"
	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

func TestMeasureLatencyPopulations(t *testing.T) {
	t.Parallel()
	everyThird := func(id uint32) bool { return id%3 != 2 }
	for _, c := range []struct {
		name     string
		answer   func(uint32) bool
		delay    time.Duration
		interval time.Duration
		window   time.Duration
		check    func(LatencyStats) bool
	}{
		{"answered", answerAll, 0, 20 * time.Millisecond, captureWindow, func(s LatencyStats) bool {
			ratio, ok := s.TimeoutRatio()
			return s.Count > 0 && s.Min > 0 && s.P50 > 0 && ok && ratio == 0
		}},
		{"silent", answerNone, 0, 20 * time.Millisecond, captureWindow, func(s LatencyStats) bool {
			ratio, ok := s.TimeoutRatio()
			return s.Count == 0 && ok && ratio == 1
		}},
		{"every third dropped", everyThird, 0, 20 * time.Millisecond, captureWindow, func(s LatencyStats) bool {
			ratio, ok := s.TimeoutRatio()
			return s.Count > 0 && s.Mean > 0 && ok && ratio >= 0.10 && ratio <= 0.55
		}},
		{"silent probes drain to their deadline", answerNone, 0, 10 * time.Millisecond, 80 * time.Millisecond,
			func(s LatencyStats) bool {
				ratio, ok := s.TimeoutRatio()
				return s.Count == 0 && s.Timeouts > 0 && s.Unresolved == 0 && ok && ratio == 1
			}},
		{"in-flight replies at window end", answerAll, 100 * time.Millisecond, 20 * time.Millisecond,
			150 * time.Millisecond, func(s LatencyStats) bool {
				return s.Count >= 7 && s.Timeouts == 0 && s.Unresolved == 0
			}},
		{"slow replies above the cadence", answerAll, 400 * time.Millisecond, 80 * time.Millisecond, time.Second,
			func(s LatencyStats) bool {
				return s.Count > 0 && s.Timeouts > 0 && s.P50 >= 400*time.Millisecond
			}},
	} {
		t.Run(c.name, func(t *testing.T) {
			t.Parallel()
			r := testRunner(newPingServer(t, c.answer, c.delay))
			r.cfg.PingInterval = c.interval
			var replies atomic.Int64
			r.emit = func(e Event) {
				if e.Kind == EventLatency && !e.Latency.TimedOut {
					replies.Add(1)
				}
			}
			stats, err := r.measureNow(t.Context(), c.window)
			if err != nil || !c.check(stats) || replies.Load() != int64(stats.Count) {
				t.Fatalf("stats = %+v, %v; %d reply events", stats, err, replies.Load())
			}
		})
	}
}

func TestMeasureLatencyRedialsAProvenBus(t *testing.T) {
	t.Parallel()
	const dropAfter = 3
	var accepted atomic.Int64
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		conn, err := websocket.Accept(w, r, &websocket.AcceptOptions{CompressionMode: websocket.CompressionDisabled})
		if err != nil {
			return
		}
		defer conn.CloseNow()
		accepted.Add(1)
		for range dropAfter {
			_, msg, err := conn.Read(r.Context())
			if err != nil {
				return
			}
			if id, err := wire.DecodePing(string(msg)); err == nil {
				_ = conn.Write(r.Context(), websocket.MessageText, []byte(wire.EncodePong(id, 0)))
			}
		}
	}))
	defer srv.Close()
	r := testRunner(srv)
	r.cfg.PingInterval = 20 * time.Millisecond
	stats, err := r.measureNow(t.Context(), time.Second)
	if err != nil || accepted.Load() < 2 || stats.Count <= dropAfter {
		t.Fatalf("redial: %d connections, %+v, %v", accepted.Load(), stats, err)
	}
}

func TestMeasureLatencyFailsPromptlyOnAnUnprovenBus(t *testing.T) {
	t.Parallel()
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if conn, err := websocket.Accept(w, r, nil); err == nil {
			_ = conn.Close(websocket.StatusNormalClosure, "")
		}
	}))
	defer srv.Close()
	r := testRunner(srv)
	begin := time.Now()
	_, err := r.measureLatency(t.Context(), StageLatency, false, 5*time.Second, testStageGate(make(chan struct{})))
	if err == nil || time.Since(begin) > 500*time.Millisecond {
		t.Fatalf("closed bus returned %v after %v", err, time.Since(begin))
	}
}

func TestRedialPingBusDoesNotRetryPermanentAuthenticationFailure(t *testing.T) {
	t.Parallel()
	var requests atomic.Int64
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		requests.Add(1)
		w.Header().Set("Graphite-Meter-Auth", "required")
		w.WriteHeader(http.StatusForbidden)
	}))
	defer srv.Close()
	_, err := testRunner(srv).redialPingBus(t.Context(), time.Now().Add(redialWindow))
	if _, ok := errors.AsType[*AuthRequiredError](err); !ok || requests.Load() != 1 {
		t.Fatalf("redial error = %v after %d requests, want AuthRequiredError after one", err, requests.Load())
	}
}

func TestProbeDeadlinesAdaptAndSeparateUnresolved(t *testing.T) {
	t.Parallel()
	now := time.Now()
	l := &probeLedger{pending: map[uint32]probe{}, late: map[uint32]time.Time{}, window: 16}
	if l.timeout() != probeTimeoutFloor {
		t.Fatalf("cold timeout = %v", l.timeout())
	}
	for range 20 {
		l.observe(2 * time.Second)
	}
	if got := l.timeout(); got < 2*time.Second || got > probeTimeoutCeil {
		t.Fatalf("timeout after slow replies = %v, want at least the observed RTT", got)
	}
	l.stats.add(10*time.Millisecond, false, 0)
	l.pending = map[uint32]probe{
		1: {sent: now.Add(-time.Second), deadline: now.Add(-time.Millisecond), measured: true},
		2: {sent: now.Add(-time.Millisecond), deadline: now.Add(time.Second), measured: true},
		3: {sent: now.Add(-time.Second), deadline: now.Add(-time.Millisecond)},
	}
	l.closePending(now)
	l.stats.add(100*time.Millisecond, false, 0)
	got := l.stats.snapshot()
	if len(l.pending) != 0 || got.Timeouts != 1 || got.Unresolved != 1 || got.JitterPairs != 0 {
		t.Fatalf("cutoff summary: %+v", got)
	}
}

func TestLatencyFailurePreservesItsMeasuredPopulation(t *testing.T) {
	t.Parallel()
	for _, stage := range []Stage{StageLatency, StageDownload} {
		for _, reply := range []bool{false, true} {
			t.Run(string(stage)+fmt.Sprint("/reply=", reply), func(t *testing.T) {
				t.Parallel()
				var accepts atomic.Int64
				srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, req *http.Request) {
					if req.URL.Path == "/download" {
						_, _ = w.Write(make([]byte, 32*1024))
						return
					}
					if accepts.Add(1) > 1 {
						w.Header().Set("Graphite-Meter-Auth", "required")
						w.WriteHeader(http.StatusForbidden)
						return
					}
					conn, err := websocket.Accept(w, req, nil)
					if err != nil {
						return
					}
					defer conn.CloseNow()
					started := time.Now()
					timer := time.AfterFunc(400*time.Millisecond, func() { _ = conn.CloseNow() })
					defer timer.Stop()
					for {
						_, msg, err := conn.Read(req.Context())
						if err != nil {
							return
						}
						f, err := wire.DecodePing(string(msg))
						if reply && err == nil && time.Since(started) <= 100*time.Millisecond {
							_ = conn.Write(req.Context(), websocket.MessageText, []byte(wire.EncodePong(f, 0)))
						}
					}
				}))
				defer srv.Close()
				var throughput *Result
				var details *RunDetails
				r := testRunner(srv)
				r.cfg.PingInterval, r.cfg.LoadedLatency = 20*time.Millisecond, stage == StageDownload
				r.cfg.LoadedPingInterval = r.cfg.PingInterval
				r.emit = func(e Event) {
					if e.Kind == EventResult {
						throughput = e.Result
					}
					if e.Kind == EventDone {
						details = e.Servers
					}
				}
				err := r.runTestStage(t.Context(), stage, time.Second)
				failed := details != nil && len(details.Failures) == 1 && details.Failures[0].Scope == "latency"
				outcome := OutcomePartial
				if stage == StageLatency && !reply {
					outcome = OutcomeIncomplete
				}
				if err != nil || !failed || details.Outcome != outcome || len(details.Participants) != 1 {
					t.Fatalf("latency failure removed throughput membership: %v %+v", err, details)
				}
				results := details.Servers[0].Results
				i := slices.IndexFunc(results, func(r Result) bool { return r.Direction == "" })
				if i < 0 || results[i].Err == nil {
					t.Fatalf("partial latency population = %+v", results)
				}
				stats := results[i].Latency
				if (stats.Count > 0) != reply || stats.Timeouts == 0 || stage == StageLatency && stats.Unresolved == 0 {
					t.Fatalf("failure discarded probe outcomes: %+v", stats)
				}
				if stats.Elapsed <= 0 || stats.Elapsed >= time.Second || results[i].Elapsed != stats.Elapsed {
					t.Fatalf("failure reports requested duration rather than measured window: %+v", results[i])
				}
				if r.cfg.LoadedLatency && (throughput == nil || throughput.Direction != Down ||
					throughput.Unavailable || throughput.TotalBytes == 0 || throughput.Err != nil) {
					t.Fatalf("latency failure discarded throughput: %+v", throughput)
				}
			})
		}
	}
}

func TestLoadedProbesKeepTwoInFlight(t *testing.T) {
	t.Parallel()
	r := testRunner(newPingServer(t, answerNone, 0))
	r.cfg.LoadedPingInterval = 10 * time.Millisecond
	start := make(chan struct{})
	close(start)
	stats, err := r.measureLatency(t.Context(), StageDownload, true, captureWindow, testStageGate(start))
	if attempts := stats.Timeouts + stats.Unresolved; err != nil || attempts == 0 || attempts > 4 {
		t.Fatalf("loaded window sent %d unanswered probes: %+v, %v", attempts, stats, err)
	}
}

func TestProbeLedgerMeasuresOnlyTheWindow(t *testing.T) {
	t.Parallel()
	start := time.Now()
	l := &probeLedger{pending: map[uint32]probe{}, late: map[uint32]time.Time{}, window: 16}
	warm, _ := l.register(start)
	l.open(start.Add(time.Second))
	inside, _ := l.register(start.Add(900 * time.Millisecond))
	late, _ := l.register(start.Add(950 * time.Millisecond))
	if _, ok := l.register(start.Add(time.Second)); ok {
		t.Fatal("a probe was sent after the window closed")
	}
	if _, _, counted := l.reply(warm, start.Add(200*time.Millisecond), 0); counted {
		t.Fatal("a warmup probe that replied inside the window was measured")
	}
	if rtt, timedOut, counted := l.reply(inside, start.Add(1050*time.Millisecond), 0); !counted || timedOut ||
		rtt != 150*time.Millisecond {
		t.Fatalf("an in-window probe draining after the window = %v %v %v, want a reply", rtt, timedOut, counted)
	}
	if _, timedOut, counted := l.reply(late, start.Add(2*time.Second), 0); !counted || !timedOut {
		t.Fatal("a reply after its deadline was not a timeout")
	}
	stats := l.finish(start.Add(3*time.Second), time.Second)
	if stats.Count != 1 || stats.Timeouts != 1 || stats.Elapsed != time.Second {
		t.Fatalf("window population = %+v", stats)
	}
}

func TestReplyDrivenProbesFollowRepliesAndTheirDeadline(t *testing.T) {
	t.Parallel()
	for _, c := range []struct {
		name   string
		answer func(uint32) bool
		check  func(LatencyStats) bool
	}{
		{"answered", answerAll, func(s LatencyStats) bool { return s.Count >= 20 }},
		{"silent", answerNone, func(s LatencyStats) bool { return s.Timeouts+s.Unresolved <= 5 }},
	} {
		r := testRunner(newPingServer(t, c.answer, 5*time.Millisecond))
		r.cfg.PingInterval = PingReplyDriven
		stats, err := r.measureNow(t.Context(), time.Second)
		if err != nil || !c.check(stats) {
			t.Errorf("%s: reply-driven window = %+v, %v", c.name, stats, err)
		}
	}
}

func TestIdleAndLoadedStagesKeepTheirOwnCadence(t *testing.T) {
	t.Parallel()
	r := testRunner(newPingServer(t, answerAll, 0))
	r.cfg.PingInterval, r.cfg.LoadedPingInterval = PingSlow, 10*time.Millisecond
	start := make(chan struct{})
	close(start)
	idle, err := r.measureLatency(t.Context(), StageLatency, false, captureWindow, testStageGate(start))
	loaded, loadedErr := r.measureLatency(t.Context(), StageDownload, true, captureWindow, testStageGate(start))
	if err != nil || loadedErr != nil || idle.Count > 2 || loaded.Count < 10 {
		t.Fatalf("idle %d replies (%v), loaded %d replies (%v)", idle.Count, err, loaded.Count, loadedErr)
	}
}
