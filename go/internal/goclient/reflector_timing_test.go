package goclient

import (
	"fmt"
	"math"
	"net/http"
	"reflect"
	"sync/atomic"
	"testing"
	"testing/synctest"
	"time"

	"github.com/coder/websocket"
	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

func TestReflectorTimingDoesNotChangeRawStatistics(t *testing.T) {
	t.Parallel()
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
	wantTiming := ReflectorTimingStats{
		Count:        2,
		MeanRawRTT:   15 * time.Millisecond,
		MeanHandling: time.Millisecond,
	}
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
	t.Parallel()
	for _, nanos := range []uint64{0, math.MaxInt64, math.MaxInt64 + 1, math.MaxUint64} {
		var stats latencyStats
		stats.add(time.Duration(math.MaxInt64), false, nanos)
		got := stats.snapshot()
		if got.Count != 1 || got.P50 != time.Duration(math.MaxInt64) || got.Timeouts != 0 {
			t.Fatalf("handling %d changed the raw reply: %+v", nanos, got)
		}
		representable := got.ReflectorTiming != nil && uint64(got.ReflectorTiming.MeanHandling) == nanos
		if representable != (nanos <= math.MaxInt64) {
			t.Fatalf("handling %d produced the diagnostic %+v", nanos, got.ReflectorTiming)
		}
	}
}

func TestNativeReflectorTimingValidationAndReconnect(t *testing.T) {
	t.Parallel()
	for _, scenario := range []string{"zero", "impossible", "reconnect"} {
		t.Run(scenario, func(t *testing.T) {
			t.Parallel()
			synctest.Test(t, func(t *testing.T) {
				var connections atomic.Int32
				r := pipedRunner(t, http.HandlerFunc(func(w http.ResponseWriter, request *http.Request) {
					conn, err := websocket.Accept(w, request, &websocket.AcceptOptions{
						CompressionMode: websocket.CompressionDisabled,
					})
					if err != nil {
						return
					}
					defer conn.CloseNow()
					generation := connections.Add(1)
					for count := 1; ; count++ {
						_, message, err := conn.Read(request.Context())
						if err != nil {
							return
						}
						frame, err := wire.DecodePing(string(message))
						if err != nil {
							continue
						}
						if scenario == "reconnect" && generation == 1 && count == 5 {
							return
						}
						value := "0"
						if scenario == "impossible" {
							value = "18446744073709551615"
						}
						pong := []byte(fmt.Sprintf("PONG,%d,%s", frame, value))
						time.Sleep(time.Millisecond)
						for range 2 {
							if err := conn.Write(request.Context(), websocket.MessageText, pong); err != nil {
								return
							}
						}
					}
				}))
				r.cfg.PingInterval, r.cfg.LoadedPingInterval = 10*time.Millisecond, 10*time.Millisecond
				samples := 0
				r.emit = func(event Event) {
					if event.Kind == EventLatency && !event.Latency.TimedOut {
						samples++
					}
				}
				stats, err := r.measureNow(t.Context(), false, 180*time.Millisecond)
				if err != nil || stats.Count == 0 || samples != stats.Count {
					t.Fatalf("raw/connection observations: stats=%+v events=%d, %v", stats, samples, err)
				}
				timing := stats.ReflectorTiming
				if scenario == "impossible" && timing != nil {
					t.Fatalf("unavailable timing manufactured a diagnostic: %+v", timing)
				}
				if scenario != "impossible" &&
					(timing == nil || timing.Count != stats.Count || timing.MeanHandling != 0) {
					t.Fatalf("paired summary=%+v replies=%d", timing, stats.Count)
				}
				if want := map[bool]int32{false: 1, true: 2}[scenario == "reconnect"]; connections.Load() != want {
					t.Fatalf("%d connections, want %d", connections.Load(), want)
				}
			})
		})
	}
}
