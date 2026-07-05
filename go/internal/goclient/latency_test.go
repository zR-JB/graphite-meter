package goclient

import (
	"context"
	"net/http"
	"net/http/httptest"
	"sync"
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

	start := make(chan struct{})
	close(start)
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()

	stats, err := r.measureLatency(ctx, "latency", false, 300*time.Millisecond, start)
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

// newSilentPingServer accepts the WebSocket and reads PINGs but never answers,
// simulating a peer that is up but not responding.
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

	start := make(chan struct{})
	close(start)
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()

	// lossAfter is max(4*PingInterval, 250ms) = 250ms here, so a 400ms window
	// gives ample margin for at least one ping to be declared lost.
	stats, err := r.measureLatency(ctx, "latency", false, 400*time.Millisecond, start)
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

// newIntermittentLossPingServer answers every PING except every dropEvery'th
// one (by ID), which it silently swallows — a mixed pattern of hits and
// misses rather than an all-respond or a never-respond peer.
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

// TestMeasureLatencyMixedLossComputesRatioAndRTT checks a mixed sequence of
// hits and misses (rather than all-or-nothing loss) still yields a partial
// loss ratio and RTT stats computed only from the responses that arrived.
func TestMeasureLatencyMixedLossComputesRatioAndRTT(t *testing.T) {
	const dropEvery = 3 // every 3rd ping (by ID) goes unanswered
	srv := newIntermittentLossPingServer(t, dropEvery)
	defer srv.Close()

	cfg := Config{BaseURL: srv.URL, PingInterval: 20 * time.Millisecond}.normalized()
	r := &runner{cfg: cfg, http: srv.Client(), emit: func(Event) {}}

	start := make(chan struct{})
	close(start)
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()

	stats, err := r.measureLatency(ctx, "latency", false, 700*time.Millisecond, start)
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

// TestMeasureLatencyClosedConnectionDoesNotHang checks that a peer dropping the
// connection before measurement even starts (start never fires, simulating a
// still-in-progress warmup) surfaces an error promptly instead of hanging.
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

	start := make(chan struct{}) // never closed: measurement never begins
	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
	defer cancel()

	type outcome struct {
		err error
	}
	done := make(chan outcome, 1)
	begin := time.Now()
	go func() {
		_, err := r.measureLatency(ctx, "latency", false, 5*time.Second, start)
		done <- outcome{err: err}
	}()

	select {
	case o := <-done:
		if o.err == nil {
			t.Error("want an error when the websocket closes before measurement starts")
		}
	case <-time.After(1 * time.Second):
		t.Fatal("measureLatency hung after the peer closed the connection")
	}
	if elapsed := time.Since(begin); elapsed > 500*time.Millisecond {
		t.Errorf("measureLatency took %v to notice the closed connection, want near-instant", elapsed)
	}
}
