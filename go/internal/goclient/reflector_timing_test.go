package goclient

import (
	"context"
	"fmt"
	"math"
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
		handling uint64
	}{
		{10 * time.Millisecond, false, uint64(2 * time.Millisecond)},
		{20 * time.Millisecond, false, 0},
		{30 * time.Millisecond, false, math.MaxUint64},
		{40 * time.Millisecond, false, uint64(41 * time.Millisecond)},
		{50 * time.Millisecond, false, math.MaxUint64},
		{250 * time.Millisecond, true, uint64(200 * time.Millisecond)},
	} {
		raw.add(row.rtt, row.timeout, math.MaxUint64)
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
	timed.add(100*time.Millisecond, false, uint64(20*time.Millisecond))
	if *captured.ReflectorTiming != wantTiming {
		t.Fatal("snapshot mutated after later observations")
	}
}

func TestReflectorTimingDurationBounds(t *testing.T) {
	for _, nanos := range []uint64{0, math.MaxInt64, math.MaxInt64 + 1, math.MaxUint64} {
		t.Run(fmt.Sprint(nanos), func(t *testing.T) {
			var stats latencyStats
			handling := stats.add(time.Duration(math.MaxInt64), false, nanos)
			got := stats.snapshot()
			if got.Count != 1 || got.Mean != time.Duration(math.MaxInt64) || got.Timeouts != 0 {
				t.Fatalf("optional duration changed raw reply: %+v", got)
			}
			if nanos > math.MaxInt64 {
				if handling != nil || got.ReflectorTiming != nil {
					t.Fatalf("unrepresentable duration produced a diagnostic: %v, %+v", handling, got.ReflectorTiming)
				}
				return
			}
			if handling == nil || uint64(*handling) != nanos || got.ReflectorTiming == nil || uint64(got.ReflectorTiming.MeanHandling) != nanos {
				t.Fatalf("representable duration was not retained: %v, %+v", handling, got.ReflectorTiming)
			}
		})
	}
}

func TestNativeReflectorTimingValidationAndReconnect(t *testing.T) {
	for _, scenario := range []string{"zero", "impossible", "reconnect"} {
		t.Run(scenario, func(t *testing.T) {
			var connections atomic.Int32
			var mu sync.Mutex
			var samples []LatencySample
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
					frame, err := wire.DecodePing(string(message))
					if err != nil {
						continue
					}
					count++
					if scenario == "reconnect" && generation == 1 && count == 5 {
						return
					}
					value := "0"
					if scenario == "impossible" {
						value = "18446744073709551615"
					}
					pong := fmt.Sprintf("PONG,%d,%s", frame, value)
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
				if event.Kind != EventLatency || event.Latency.TimedOut {
					return
				}
				mu.Lock()
				defer mu.Unlock()
				samples = append(samples, event.Latency)
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
			if stats.Count == 0 || len(samples) != stats.Count {
				t.Fatalf("raw/connection observations: stats=%+v events=%d", stats, len(samples))
			}
			paired := 0
			for _, sample := range samples {
				if sample.ReflectorHandling != nil {
					paired++
				}
			}
			if scenario == "impossible" {
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
				if scenario == "reconnect" && (connections.Load() != 2 || paired != stats.Count) {
					t.Fatalf("reconnect failed to retain timing: connections=%d paired=%d replies=%d", connections.Load(), paired, stats.Count)
				}
			}
		})
	}
}
