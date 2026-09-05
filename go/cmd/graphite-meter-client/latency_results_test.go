package main

import (
	"github.com/zR-JB/graphite-meter/go/internal/goclient"
	"strings"
	"testing"
	"time"
)

func TestLatencyResultsShowAvailabilityAndProbeTerminology(t *testing.T) {
	for _, tt := range []struct {
		name  string
		stats goclient.LatencyStats
		want  string
	}{
		{"empty", goclient.LatencyStats{}, "probe timeouts -- (no resolved probes)"},
		{"all expired", goclient.LatencyStats{Timeouts: 3}, "probe timeouts 100.0% (3/3)"},
		{"zero variation", goclient.LatencyStats{Count: 2, JitterPairs: 1}, "RTT variation 0.00 ms"},
		{"no variation pair", goclient.LatencyStats{Count: 1}, "RTT variation --"},
		{"incomplete", goclient.LatencyStats{Unresolved: 2, SendFailures: 1}, "unresolved 2  send failures 1"},
	} {
		t.Run(tt.name, func(t *testing.T) {
			got := latencyOutcomeSummary(tt.stats)
			if !strings.Contains(got, tt.want) || strings.Contains(got, "loss") {
				t.Fatalf("summary %q, want %q", got, tt.want)
			}
		})
	}
}

func TestTimeoutOnlyLoadedResultIsLatency(t *testing.T) {
	result := goclient.Result{Stage: "upload", Latency: goclient.LatencyStats{Timeouts: 3, TimeoutAfter: time.Second}}
	if !isLatencyResult(result) {
		t.Fatal("timeout-only loaded result classified as throughput")
	}
	if isLatencyResult(goclient.Result{Stage: "upload", Direction: goclient.Up}) {
		t.Fatal("upload result classified as latency")
	}
}
