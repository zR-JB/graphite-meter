package main

import (
	"errors"
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

func TestIncompleteLatencyResultIsStoredWithoutCompletingTheStage(t *testing.T) {
	failure := errors.New("latency connection failed")
	m := newModel(goclient.DefaultConfig())
	m.stages = []stageProgress{{name: "latency", state: stageMeasuring}}
	result := goclient.Result{Stage: "latency", Latency: goclient.LatencyStats{Timeouts: 2, Unresolved: 1}, Err: failure}
	m.apply(goclient.Event{Kind: goclient.EventResult, Result: &result})
	m, _ = modelAndCmd(m.Update(eventsMsg{events: []goclient.Event{{Kind: goclient.EventDone, Err: failure}}}))
	if len(m.results) != 1 || !errors.Is(m.results[0].Err, failure) || m.stages[0].state != stageStopped || !errors.Is(m.err, failure) {
		t.Fatalf("partial result lost or marked complete: results=%+v stages=%+v err=%v", m.results, m.stages, m.err)
	}
	if got := m.resultsView(120); !strings.Contains(got, "Incomplete: latency connection failed") || !strings.Contains(got, "unresolved 1") {
		t.Fatalf("missing partial disclosure: %q", got)
	}
}

func TestFinalReportRetainsIncompleteLatencyOnFailure(t *testing.T) {
	failure := errors.New("latency connection failed")
	m := newModel(goclient.DefaultConfig())
	m.err = failure
	m.results = []goclient.Result{{Stage: "latency", Latency: goclient.LatencyStats{Timeouts: 1}, Err: failure}}
	if got := m.finalReport(); !strings.Contains(got, "Incomplete: latency connection failed") {
		t.Fatalf("partial failure report was discarded: %q", got)
	}
}
