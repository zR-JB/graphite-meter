package goclient

import (
	"context"
	"errors"
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
	if latencyEvents == 0 {
		t.Error("no non-lost EventLatency samples emitted")
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

func TestMeasureLatencyRegistersLossWithoutResponse(t *testing.T) {
	srv := newSilentPingServer(t)
	defer srv.Close()

	cfg := Config{BaseURL: srv.URL, PingInterval: 20 * time.Millisecond}.normalized()
	r := &runner{cfg: cfg, http: srv.Client(), emit: func(Event) {}}
	attachTestLatencyTarget(r, srv.URL)

	start := make(chan struct{})
	close(start)
	ctx, cancel := context.WithTimeout(t.Context(), 10*time.Second)
	defer cancel()

	// lossAfter is max(4*PingInterval, 250ms) = 250ms here, well inside the window.
	stats, err := r.measureLatency(ctx, "latency", false, captureWindow, start)
	if err != nil {
		t.Fatalf("measureLatency: %v", err)
	}
	if stats.Count != 0 {
		t.Errorf("Count = %d, want 0 (no responses were ever sent)", stats.Count)
	}
	if stats.Loss != 1 {
		t.Errorf("Loss = %v, want 1 (total loss)", stats.Loss)
	}
}

func newIntermittentLossPingServer(t *testing.T, dropEvery uint32) *httptest.Server {
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

func TestMeasureLatencyMixedLossComputesRatioAndRTT(t *testing.T) {
	const dropEvery = 3 // every 3rd ping (by ID) goes unanswered
	srv := newIntermittentLossPingServer(t, dropEvery)
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
		t.Fatal("want some successful RTT samples in a mixed-loss run")
	}
	if stats.Min <= 0 || stats.Mean <= 0 || stats.P50 <= 0 {
		t.Errorf("want positive RTT stats from the responded pings, got %+v", stats)
	}
	if stats.Loss <= 0 || stats.Loss >= 1 {
		t.Errorf("Loss = %v, want a partial ratio strictly between 0 and 1 for a mixed hit/miss sequence", stats.Loss)
	}
	if stats.Loss < 0.10 || stats.Loss > 0.55 {
		t.Errorf("Loss = %v, want roughly 1/%d given the drop pattern", stats.Loss, dropEvery)
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
	if stats.Loss == 1 {
		t.Errorf("Loss = %v, want the redialled bus's answers to count", stats.Loss)
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
