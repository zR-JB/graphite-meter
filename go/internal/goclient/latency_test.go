package goclient

import (
	"errors"
	"fmt"
	"net/http"
	"net/http/httptest"
	"slices"
	"sync/atomic"
	"testing"
	"testing/synctest"
	"time"

	"github.com/coder/websocket"
	"github.com/quic-go/webtransport-go"
	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

// Each population runs in virtual time, so its counts are exact rather than a scheduling-tolerant band.
func TestMeasureLatencyPopulations(t *testing.T) {
	t.Parallel()
	everyThird := func(id uint32) bool { return id%3 != 2 }
	ms := time.Millisecond
	for _, c := range []struct {
		name                        string
		answer                      func(uint32) bool
		delay, interval, window     time.Duration
		loaded                      bool
		count, timeouts, unresolved int
		p50                         time.Duration
	}{
		{"answered", answerAll, ms, 20 * ms, captureWindow, false, 14, 0, 0, ms},
		{"silent", answerNone, 0, 20 * ms, captureWindow, false, 0, 14, 0, 0},
		{"every third dropped", everyThird, ms, 20 * ms, captureWindow, false, 9, 5, 0, ms},
		{"silent probes drain to their deadline", answerNone, 0, 10 * ms, 80 * ms, false, 0, 7, 0, 0},
		{"in-flight replies at window end", answerAll, 105 * ms, 20 * ms, 150 * ms, false, 7, 0, 0, 105 * ms},
		{"slow replies above the cadence", answerAll, 395 * ms, 80 * ms, time.Second, false, 8, 4, 0, 395 * ms},
		{"idle keeps its own cadence", answerAll, 0, PingSlow, time.Second, false, 1, 0, 0, 0},
		{"loaded keeps its own cadence", answerAll, 0, 10 * ms, time.Second, true, 99, 0, 0, 0},
		{"loaded silent probes keep two in flight", answerNone, 0, 10 * ms, captureWindow, true, 0, 2, 0, 0},
		{"reply-driven follows replies", answerAll, 5 * ms, PingReplyDriven, time.Second, false, 199, 0, 0, 5 * ms},
		{"reply-driven silent waits for deadlines", answerNone, 0, PingReplyDriven, time.Second, false, 0, 3, 0, 0},
	} {
		t.Run(c.name, func(t *testing.T) {
			t.Parallel()
			synctest.Test(t, func(t *testing.T) {
				r := pipedRunner(t, pingHandler(c.answer, c.delay))
				r.cfg.PingInterval, r.cfg.LoadedPingInterval = c.interval, PingSlow
				if c.loaded {
					r.cfg.PingInterval, r.cfg.LoadedPingInterval = PingSlow, c.interval
				}
				var replies atomic.Int64
				r.emit = func(e Event) {
					if e.Kind == EventLatency && !e.Latency.TimedOut {
						replies.Add(1)
					}
				}
				s, err := r.measureNow(t.Context(), c.loaded, c.window)
				if err != nil || s.Count != c.count || s.Timeouts != c.timeouts || s.Unresolved != c.unresolved ||
					s.P50 != c.p50 || replies.Load() != int64(s.Count) {
					t.Fatalf("stats = %+v, %v; %d reply events", s, err, replies.Load())
				}
			})
		})
	}
}

func TestLaneEndingsNameTheirReason(t *testing.T) {
	t.Parallel()
	for _, c := range []struct {
		code     websocket.StatusCode
		session  webtransport.SessionErrorCode
		want     FailureReason
		redialed bool
	}{
		{4001, 1, FailureTimeout, true},
		{4002, 2, FailureTimeout, true},
		{1001, 4, FailureConnectionLost, true},
		{1008, 3, FailureSignIn, false},
	} {
		synctest.Test(t, func(t *testing.T) {
			var accepted atomic.Int64
			r := pipedRunner(t, http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				if accepted.Add(1) > 1 {
					pingHandler(answerAll, time.Millisecond).ServeHTTP(w, r)
					return
				}
				conn, err := websocket.Accept(w, r, nil)
				if err != nil {
					return
				}
				_, msg, _ := conn.Read(r.Context())
				time.Sleep(time.Millisecond)
				if id, err := wire.DecodePing(string(msg)); err == nil {
					_ = conn.Write(r.Context(), websocket.MessageText, []byte(wire.EncodePong(id, 0)))
				}
				_ = conn.Close(c.code, "")
			}))
			r.cfg.PingInterval = 20 * time.Millisecond
			stats, err := r.measureNow(t.Context(), false, time.Second)
			if (err == nil) != c.redialed || (accepted.Load() > 1) != c.redialed || c.redialed && stats.Count < 2 {
				t.Errorf("close %d: %v after %d connections, %d replies", c.code, err, accepted.Load(), stats.Count)
			}
			closed := &webtransport.SessionError{Remote: true, ErrorCode: c.session}
			if got := failureReason(laneEnding(websocket.CloseError{Code: c.code}), false); got != c.want ||
				failureReason(laneEnding(closed), false) != c.want {
				t.Errorf("close %d reads as %s, want %s", c.code, got, c.want)
			}
		})
	}
}

func TestLaneEndingReadsEveryWireEnding(t *testing.T) {
	t.Parallel()
	for _, want := range wire.LaneEnds {
		for _, closed := range []error{websocket.CloseError{Code: websocket.StatusCode(want.WS)},
			&webtransport.SessionError{Remote: true, ErrorCode: webtransport.SessionErrorCode(want.WT)}} {
			got := laneEnding(closed)
			end, ended := errors.AsType[laneEnd](got)
			_, auth := errors.AsType[*AuthRequiredError](got)
			wrong := !ended || end.Name != want.Name
			switch want {
			case wire.LaneFinished:
				wrong = got != closed
			case wire.LaneRevoked:
				wrong = !auth
			}
			if wrong {
				t.Errorf("%s ending %v reads as %v", want.Name, closed, got)
			}
		}
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

func TestClosingProbesSeparatesTimeoutsFromUnresolved(t *testing.T) {
	t.Parallel()
	now := time.Now()
	l := &probeLedger{pending: map[uint32]probe{
		1: {sent: now.Add(-time.Second), deadline: now.Add(-time.Millisecond), measured: true},
		2: {sent: now.Add(-time.Millisecond), deadline: now.Add(time.Second), measured: true},
		3: {sent: now.Add(-time.Second), deadline: now.Add(-time.Millisecond)},
	}}
	l.stats.add(10*time.Millisecond, false, 0)
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
				synctest.Test(t, func(t *testing.T) {
					var accepts atomic.Int64
					r := pipedRunner(t, http.HandlerFunc(func(w http.ResponseWriter, req *http.Request) {
						if req.URL.Path == "/download" {
							time.Sleep(time.Millisecond)
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
					r.cfg.PingInterval, r.cfg.LoadedLatency = 20*time.Millisecond, stage == StageDownload
					r.cfg.LoadedPingInterval = r.cfg.PingInterval
					var log eventLog
					r.emit = log.emit
					err := r.runTestStage(t.Context(), stage, time.Second)
					details := log.details()
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
					if (stats.Count > 0) != reply || stats.Timeouts == 0 ||
						stage == StageLatency && stats.Unresolved == 0 {
						t.Fatalf("failure discarded probe outcomes: %+v", stats)
					}
					if stats.Elapsed <= 0 || stats.Elapsed >= time.Second || results[i].Elapsed != stats.Elapsed {
						t.Fatalf("failure reports requested duration rather than measured window: %+v", results[i])
					}
					if throughput := log.results(); r.cfg.LoadedLatency && (len(throughput) != 1 ||
						throughput[0].Direction != Down || throughput[0].Unavailable || throughput[0].TotalBytes == 0 ||
						throughput[0].Err != nil) {
						t.Fatalf("latency failure discarded throughput: %+v", throughput)
					}
				})
			})
		}
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
