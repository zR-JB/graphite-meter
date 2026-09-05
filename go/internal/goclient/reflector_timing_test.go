package goclient

import (
	"context"
	"fmt"
	"net/http"
	"net/http/httptest"
	"reflect"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"github.com/coder/websocket"
	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

func TestReflectorTimingDoesNotChangeRawStatistics(t *testing.T) {
	var raw, timed latencyStats
	for _, row := range []struct {
		rtt      time.Duration
		timeout  bool
		handling *uint64
	}{
		{10 * time.Millisecond, false, new(uint64(2 * time.Millisecond))},
		{20 * time.Millisecond, false, new(uint64(0))},
		{30 * time.Millisecond, false, nil},
		{40 * time.Millisecond, false, new(uint64(41 * time.Millisecond))},
		{50 * time.Millisecond, false, new(^uint64(0))},
		{250 * time.Millisecond, true, new(uint64(200 * time.Millisecond))},
	} {
		raw.add(row.rtt, row.timeout, nil)
		timed.add(row.rtt, row.timeout, row.handling)
	}
	got := timed.snapshot()
	wantTiming := ReflectorTimingStats{Count: 2, MeanRawRTT: 15 * time.Millisecond, MeanHandling: time.Millisecond, MeanAdjustedRTT: 14 * time.Millisecond}
	if got.ReflectorTiming == nil || *got.ReflectorTiming != wantTiming {
		t.Fatalf("timing = %+v, want %+v", got.ReflectorTiming, wantTiming)
	}
	got.ReflectorTiming = nil
	if want := raw.snapshot(); !reflect.DeepEqual(got, want) {
		t.Fatalf("raw statistics changed: %+v != %+v", got, want)
	}
	captured := timed.snapshot()
	timed.add(100*time.Millisecond, false, new(uint64(20*time.Millisecond)))
	if *captured.ReflectorTiming != wantTiming {
		t.Fatal("snapshot mutated after later observations")
	}
}

func TestNativeReflectorTimingNegotiationAndReconnect(t *testing.T) {
	for _, scenario := range []string{"legacy", "negotiated", "malformed", "reconnect"} {
		t.Run(scenario, func(t *testing.T) {
			var connections atomic.Int32
			var mu sync.Mutex
			var samples []LatencySample
			var staleTiming bool
			srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, request *http.Request) {
				conn, err := websocket.Accept(w, request, &websocket.AcceptOptions{CompressionMode: websocket.CompressionDisabled})
				if err != nil {
					return
				}
				defer conn.CloseNow()
				generation := connections.Add(1)
				count := 0
				for {
					_, message, err := conn.Read(request.Context())
					if err != nil {
						return
					}
					frame, err := wire.Decode(string(message))
					if err != nil {
						continue
					}
					if frame.Op == wire.OpHI {
						if !frame.Timing {
							t.Error("native client did not request optional timing")
						}
						timing := scenario != "legacy" && (scenario != "reconnect" || generation == 1)
						if err := conn.Write(request.Context(), websocket.MessageText, []byte(wire.Encode(wire.Frame{Op: wire.OpREADY, Timing: timing}))); err != nil {
							return
						}
						continue
					}
					if frame.Op != wire.OpPING {
						continue
					}
					count++
					if scenario == "reconnect" && generation == 1 && count == 5 {
						return
					}
					value := "0"
					if scenario == "malformed" {
						value = "-1"
					}
					pong := fmt.Sprintf("PONG,%d;TIME,0;HANDLING,%s", frame.ID, value)
					if err := conn.Write(request.Context(), websocket.MessageText, []byte(pong)); err != nil {
						return
					}
					// A duplicate echo cannot add another raw or paired observation.
					if err := conn.Write(request.Context(), websocket.MessageText, []byte(pong)); err != nil {
						return
					}
				}
			}))
			defer srv.Close()
			r := &runner{cfg: Config{BaseURL: srv.URL, PingInterval: 10 * time.Millisecond}.normalized(), http: srv.Client(), emit: func(event Event) {
				if event.Kind != EventLatency || event.Latency.Lost {
					return
				}
				mu.Lock()
				defer mu.Unlock()
				samples = append(samples, event.Latency)
				if scenario == "reconnect" && connections.Load() > 1 && event.Latency.ReflectorHandling != nil {
					staleTiming = true
				}
			}}
			attachTestLatencyTarget(r, srv.URL)
			start := make(chan struct{})
			close(start)
			ctx, cancel := context.WithTimeout(t.Context(), 2*time.Second)
			defer cancel()
			stats, err := r.measureLatency(ctx, "latency", false, 180*time.Millisecond, testStageGate(start))
			if err != nil {
				t.Fatal(err)
			}
			mu.Lock()
			defer mu.Unlock()
			if stats.Count == 0 || len(samples) != stats.Count || staleTiming {
				t.Fatalf("raw/connection observations: stats=%+v events=%d stale=%v", stats, len(samples), staleTiming)
			}
			paired := 0
			for _, sample := range samples {
				if sample.ReflectorHandling != nil {
					paired++
				}
			}
			if scenario == "legacy" || scenario == "malformed" {
				if stats.ReflectorTiming != nil || paired != 0 {
					t.Fatalf("unavailable timing manufactured a diagnostic: %+v", stats.ReflectorTiming)
				}
			} else {
				if stats.ReflectorTiming == nil || stats.ReflectorTiming.Count != paired || paired == 0 {
					t.Fatalf("paired summary=%+v events=%d", stats.ReflectorTiming, paired)
				}
				if stats.ReflectorTiming.MeanHandling != 0 || stats.ReflectorTiming.MeanAdjustedRTT != stats.ReflectorTiming.MeanRawRTT {
					t.Fatalf("zero handling altered RTT: %+v", stats.ReflectorTiming)
				}
				if scenario == "reconnect" && (connections.Load() != 2 || paired == stats.Count) {
					t.Fatalf("reconnect failed to clear capability: connections=%d paired=%d replies=%d", connections.Load(), paired, stats.Count)
				}
			}
		})
	}
}
