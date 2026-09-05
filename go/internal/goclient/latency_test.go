package goclient

import (
	"context"
	"errors"
	"fmt"
	"net/http"
	"net/http/httptest"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"github.com/coder/websocket"
	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

func TestMeasureLatencyRecordsRTTSamples(t *testing.T) {
	srv := newEchoPingServer(t)
	defer srv.Close()

	cfg := Config{BaseURL: srv.URL, PingInterval: 20 * time.Millisecond}.normalized()
	var mu sync.Mutex
	var latencyEvents int
	r := &runner{cfg: cfg, http: srv.Client(), emit: func(e Event) {
		if e.Kind == EventLatency && !e.Latency.Lost {
			mu.Lock()
			latencyEvents++
			mu.Unlock()
		}
	}}
	attachTestLatencyTarget(r, srv.URL)

	start := make(chan struct{})
	close(start)
	ctx, cancel := context.WithTimeout(t.Context(), 10*time.Second)
	defer cancel()

	stats, err := r.measureLatency(ctx, "latency", false, captureWindow, start)
	if err != nil {
		t.Fatalf("measureLatency: %v", err)
	}
	if stats.Count == 0 {
		t.Fatal("no RTT samples recorded")
	}
	if stats.Min <= 0 || stats.P50 <= 0 {
		t.Errorf("want positive RTT stats, got %+v", stats)
	}
	mu.Lock()
	defer mu.Unlock()
	if latencyEvents != stats.Count {
		t.Errorf("returned %d replies but delivered %d events", stats.Count, latencyEvents)
	}
}

func newSilentPingServer(t *testing.T) *httptest.Server {
	t.Helper()
	mux := http.NewServeMux()
	mux.HandleFunc("/ws/ping", func(w http.ResponseWriter, r *http.Request) {
		conn, err := websocket.Accept(w, r, &websocket.AcceptOptions{CompressionMode: websocket.CompressionDisabled})
		if err != nil {
			return
		}
		defer conn.Close(websocket.StatusNormalClosure, "")
		ctx := r.Context()
		for {
			if _, _, err := conn.Read(ctx); err != nil {
				return
			}
		}
	})
	return httptest.NewServer(mux)
}

func TestMeasureLatencyRegistersTimeoutsWithoutResponse(t *testing.T) {
	srv := newSilentPingServer(t)
	defer srv.Close()

	cfg := Config{BaseURL: srv.URL, PingInterval: 20 * time.Millisecond}.normalized()
	r := &runner{cfg: cfg, http: srv.Client(), emit: func(Event) {}}
	attachTestLatencyTarget(r, srv.URL)

	start := make(chan struct{})
	close(start)
	ctx, cancel := context.WithTimeout(t.Context(), 10*time.Second)
	defer cancel()

	// timeoutAfter is max(4*PingInterval, 250ms) = 250ms here, well inside the window.
	stats, err := r.measureLatency(ctx, "latency", false, captureWindow, start)
	if err != nil {
		t.Fatalf("measureLatency: %v", err)
	}
	if stats.Count != 0 {
		t.Errorf("Count = %d, want 0 (no responses were ever sent)", stats.Count)
	}
	if timeoutRatio(t, stats) != 1 {
		t.Errorf("TimeoutRatio = %v, want 1 (all resolved probes expired)", timeoutRatio(t, stats))
	}
}

func newIntermittentTimeoutPingServer(t *testing.T, dropEvery uint32) *httptest.Server {
	t.Helper()
	mux := http.NewServeMux()
	mux.HandleFunc("/ws/ping", func(w http.ResponseWriter, r *http.Request) {
		conn, err := websocket.Accept(w, r, &websocket.AcceptOptions{CompressionMode: websocket.CompressionDisabled})
		if err != nil {
			return
		}
		defer conn.Close(websocket.StatusNormalClosure, "")
		ctx := r.Context()
		for {
			_, msg, err := conn.Read(ctx)
			if err != nil {
				return
			}
			f, derr := wire.Decode(string(msg))
			if derr != nil || f.Op != wire.OpPING {
				continue
			}
			if f.ID%dropEvery == dropEvery-1 {
				continue // silently drop this one; no PONG sent
			}
			pong := wire.Encode(wire.Frame{Op: wire.OpPONG, ID: f.ID, Nanos: uint64(time.Now().UnixNano())})
			if err := conn.Write(ctx, websocket.MessageText, []byte(pong)); err != nil {
				return
			}
		}
	})
	return httptest.NewServer(mux)
}

func TestMeasureLatencyMixedTimeoutsComputeRatioAndRTT(t *testing.T) {
	const dropEvery = 3 // every 3rd ping (by ID) goes unanswered
	srv := newIntermittentTimeoutPingServer(t, dropEvery)
	defer srv.Close()

	cfg := Config{BaseURL: srv.URL, PingInterval: 20 * time.Millisecond}.normalized()
	r := &runner{cfg: cfg, http: srv.Client(), emit: func(Event) {}}
	attachTestLatencyTarget(r, srv.URL)

	start := make(chan struct{})
	close(start)
	ctx, cancel := context.WithTimeout(t.Context(), 10*time.Second)
	defer cancel()

	stats, err := r.measureLatency(ctx, "latency", false, captureWindow, start)
	if err != nil {
		t.Fatalf("measureLatency: %v", err)
	}
	if stats.Count == 0 {
		t.Fatal("want some successful RTT samples in a mixed-outcome run")
	}
	if stats.Min <= 0 || stats.Mean <= 0 || stats.P50 <= 0 {
		t.Errorf("want positive RTT stats from the responded pings, got %+v", stats)
	}
	if timeoutRatio(t, stats) <= 0 || timeoutRatio(t, stats) >= 1 {
		t.Errorf("TimeoutRatio = %v, want a partial ratio strictly between 0 and 1 for a mixed hit/miss sequence", timeoutRatio(t, stats))
	}
	if timeoutRatio(t, stats) < 0.10 || timeoutRatio(t, stats) > 0.55 {
		t.Errorf("TimeoutRatio = %v, want roughly 1/%d given the drop pattern", timeoutRatio(t, stats), dropEvery)
	}
}

func newDroppingPingServer(t *testing.T, dropAfter int) (*httptest.Server, *atomic.Int64) {
	t.Helper()
	var accepted atomic.Int64
	mux := http.NewServeMux()
	mux.HandleFunc("/ws/ping", func(w http.ResponseWriter, r *http.Request) {
		conn, err := websocket.Accept(w, r, &websocket.AcceptOptions{CompressionMode: websocket.CompressionDisabled})
		if err != nil {
			return
		}
		defer func() { _ = conn.CloseNow() }()
		accepted.Add(1)
		ctx := r.Context()
		for sent := 0; sent < dropAfter; {
			_, msg, err := conn.Read(ctx)
			if err != nil {
				return
			}
			f, derr := wire.Decode(string(msg))
			if derr != nil || f.Op != wire.OpPING {
				continue
			}
			pong := wire.Encode(wire.Frame{Op: wire.OpPONG, ID: f.ID, Nanos: uint64(time.Now().UnixNano())})
			if err := conn.Write(ctx, websocket.MessageText, []byte(pong)); err != nil {
				return
			}
			sent++
		}
	})
	return httptest.NewServer(mux), &accepted
}

func TestMeasureLatencyRedialsAProvenBus(t *testing.T) {
	const dropAfter = 3
	srv, accepted := newDroppingPingServer(t, dropAfter)
	defer srv.Close()

	cfg := Config{BaseURL: srv.URL, PingInterval: 20 * time.Millisecond}.normalized()
	r := &runner{cfg: cfg, http: srv.Client(), emit: func(Event) {}}
	attachTestLatencyTarget(r, srv.URL)

	start := make(chan struct{})
	close(start)
	ctx, cancel := context.WithTimeout(t.Context(), 10*time.Second)
	defer cancel()

	stats, err := r.measureLatency(ctx, "latency", false, captureWindow, start)
	if err != nil {
		t.Fatalf("measureLatency: %v", err)
	}
	if got := accepted.Load(); got < 2 {
		t.Fatalf("server accepted %d connections, want the bus redialled at least once", got)
	}
	if stats.Count <= dropAfter {
		t.Errorf("Count = %d, want more than the %d samples one bus answers before dropping", stats.Count, dropAfter)
	}
	if timeoutRatio(t, stats) == 1 {
		t.Errorf("TimeoutRatio = %v, want the redialled bus's answers to count", timeoutRatio(t, stats))
	}
}

func TestMeasureLatencyClosedConnectionDoesNotHang(t *testing.T) {
	mux := http.NewServeMux()
	mux.HandleFunc("/ws/ping", func(w http.ResponseWriter, r *http.Request) {
		conn, err := websocket.Accept(w, r, &websocket.AcceptOptions{CompressionMode: websocket.CompressionDisabled})
		if err != nil {
			return
		}
		_ = conn.Close(websocket.StatusNormalClosure, "")
	})
	srv := httptest.NewServer(mux)
	defer srv.Close()

	cfg := Config{BaseURL: srv.URL, PingInterval: 20 * time.Millisecond}.normalized()
	r := &runner{cfg: cfg, http: srv.Client(), emit: func(Event) {}}
	attachTestLatencyTarget(r, srv.URL)

	start := make(chan struct{}) // never closed: measurement never begins
	ctx, cancel := context.WithTimeout(t.Context(), 2*time.Second)
	defer cancel()

	done := make(chan error, 1)
	begin := time.Now()
	go func() {
		_, err := r.measureLatency(ctx, "latency", false, 5*time.Second, start)
		done <- err
	}()

	select {
	case err := <-done:
		if err == nil {
			t.Error("want an error when the WebSocket closes before measurement starts")
		}
	case <-time.After(1 * time.Second):
		t.Fatal("measureLatency hung after the peer closed the connection")
	}
	if elapsed := time.Since(begin); elapsed > 500*time.Millisecond {
		t.Errorf("measureLatency took %v to notice the closed connection, want near-instant", elapsed)
	}
}

func TestRedialPingBusDoesNotRetryPermanentAuthenticationFailure(t *testing.T) {
	var requests atomic.Int64
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		requests.Add(1)
		w.Header().Set("Graphite-Meter-Auth", "required")
		w.Header().Set("Graphite-Meter-Auth-URL", "/auth/start")
		w.WriteHeader(http.StatusForbidden)
	}))
	defer srv.Close()

	r := &runner{cfg: DefaultConfig(), emit: func(Event) {}}
	attachTestLatencyTarget(r, srv.URL)
	_, _, err := r.redialPingBus(t.Context(), time.Now().Add(busRedialWindow))
	if _, ok := errors.AsType[*AuthRequiredError](err); !ok {
		t.Fatalf("redial error = %v, want AuthRequiredError", err)
	}
	if got := requests.Load(); got != 1 {
		t.Fatalf("permanent auth refusal made %d requests, want one without retries", got)
	}
}

func TestPendingProbeCutoffPreservesUnresolved(t *testing.T) {
	now := time.Now()
	var stats latencyStats
	stats.add(10*time.Millisecond, false)
	pending := map[uint32]time.Time{1: now.Add(-time.Second), 2: now.Add(-250 * time.Millisecond), 3: now.Add(-time.Millisecond)}
	stats.closePending(pending, now, 250*time.Millisecond)
	stats.add(100*time.Millisecond, false)
	got := stats.snapshot()
	if len(pending) != 0 || got.Timeouts != 2 || got.Unresolved != 1 || got.JitterPairs != 0 {
		t.Fatalf("cutoff summary: %+v, pending=%v", got, pending)
	}
}

func TestMeasureLatencyShortWindowReportsUnresolvedInsteadOfZeroTimeoutCertainty(t *testing.T) {
	srv := newSilentPingServer(t)
	defer srv.Close()
	r := &runner{cfg: Config{BaseURL: srv.URL, PingInterval: 10 * time.Millisecond}.normalized(), http: srv.Client(), emit: func(Event) {}}
	attachTestLatencyTarget(r, srv.URL)
	start := make(chan struct{})
	close(start)
	stats, err := r.measureLatency(t.Context(), "latency", false, 80*time.Millisecond, start)
	if err != nil {
		t.Fatal(err)
	}
	if stats.Count != 0 || stats.Timeouts != 0 || stats.Unresolved == 0 || stats.TimeoutAfter != 250*time.Millisecond {
		t.Fatalf("short window: %+v", stats)
	}
	if _, ok := stats.TimeoutRatio(); ok {
		t.Fatal("unresolved probes manufactured a 0% timeout observation")
	}
}

func TestMeasureLatencyRejectsRepliesAfterTheirDeadline(t *testing.T) {
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, req *http.Request) {
		conn, err := websocket.Accept(w, req, &websocket.AcceptOptions{CompressionMode: websocket.CompressionDisabled})
		if err != nil {
			return
		}
		ctx, cancel := context.WithCancel(req.Context())
		defer cancel()
		defer conn.CloseNow()
		for {
			_, msg, err := conn.Read(ctx)
			if err != nil {
				return
			}
			frame, err := wire.Decode(string(msg))
			if err != nil || frame.Op != wire.OpPING {
				continue
			}
			go func() {
				timer := time.NewTimer(265 * time.Millisecond)
				defer timer.Stop()
				select {
				case <-ctx.Done():
					return
				case <-timer.C:
					_ = conn.Write(ctx, websocket.MessageText, []byte(wire.Encode(wire.Frame{Op: wire.OpPONG, ID: frame.ID})))
				}
			}()
		}
	}))
	defer srv.Close()
	r := &runner{cfg: Config{BaseURL: srv.URL, PingInterval: 20 * time.Millisecond}.normalized(), http: srv.Client(), emit: func(Event) {}}
	attachTestLatencyTarget(r, srv.URL)
	start := make(chan struct{})
	close(start)
	stats, err := r.measureLatency(t.Context(), "latency", false, 600*time.Millisecond, start)
	if err != nil {
		t.Fatal(err)
	}
	if stats.Count != 0 || stats.Timeouts == 0 || stats.JitterPairs != 0 {
		t.Fatalf("late replies became RTT observations: %+v", stats)
	}
}

func TestLatencyFailurePreservesItsMeasuredPopulation(t *testing.T) {
	for _, reply := range []bool{false, true} {
		t.Run(fmt.Sprint("reply=", reply), func(t *testing.T) {
			var accepts atomic.Int64
			srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, req *http.Request) {
				if accepts.Add(1) > 1 {
					w.Header().Set("Graphite-Meter-Auth", "required")
					w.WriteHeader(http.StatusForbidden)
					return
				}
				conn, err := websocket.Accept(w, req, &websocket.AcceptOptions{CompressionMode: websocket.CompressionDisabled})
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
					f, err := wire.Decode(string(msg))
					if !reply || err != nil || f.Op != wire.OpPING || time.Since(started) > 100*time.Millisecond {
						continue
					}
					_ = conn.Write(req.Context(), websocket.MessageText, []byte(wire.Encode(wire.Frame{Op: wire.OpPONG, ID: f.ID})))
				}
			}))
			defer srv.Close()
			var results []Result
			r := &runner{cfg: Config{BaseURL: srv.URL, PingInterval: 20 * time.Millisecond}.normalized(), http: srv.Client(), emit: func(e Event) {
				if e.Kind == EventResult {
					results = append(results, *e.Result)
				}
			}}
			attachTestLatencyTarget(r, srv.URL)
			err := r.runLatencyStage(t.Context(), "latency", false, 3*time.Second)
			if err == nil || len(results) != 1 || !errors.Is(results[0].Err, err) {
				t.Fatalf("error=%v; partial results=%+v", err, results)
			}
			stats := results[0].Latency
			if (stats.Count > 0) != reply || stats.Timeouts == 0 || stats.Unresolved == 0 {
				t.Fatalf("failure discarded probe outcomes: %+v", stats)
			}
			if stats.Elapsed <= 0 || stats.Elapsed >= 3*time.Second || results[0].Elapsed != stats.Elapsed {
				t.Fatalf("failure reports requested duration rather than measured window: %+v", results[0])
			}
		})
	}
}
