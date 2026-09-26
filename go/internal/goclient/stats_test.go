package goclient

import (
	"testing"
	"time"
)

// P50 is the midpoint median; P10/P90/P95 use nearest rank.
func TestPercentiles(t *testing.T) {
	t.Parallel()
	four := []time.Duration{10, 20, 30, 40}
	for _, c := range []struct {
		xs   []time.Duration
		p    float64
		want time.Duration
	}{
		{nil, 0.5, 0},
		{[]time.Duration{42}, 0.95, 42},
		{four, 0, 10}, {four, 0.1, 10}, {four, 0.9, 40}, {four, 0.95, 40}, {four, 1, 40},
	} {
		if got := percentile(c.xs, c.p); got != c.want {
			t.Errorf("percentile(%v, %v) = %v, want %v", c.xs, c.p, got, c.want)
		}
	}
	if got := median(four); got != 25 {
		t.Errorf("median(%v) = %v, want the midpoint 25", four, got)
	}
}

func timeoutRatio(t *testing.T, s LatencyStats) float64 {
	t.Helper()
	ratio, ok := s.TimeoutRatio()
	if !ok {
		t.Fatal("timeout ratio unavailable")
	}
	return ratio
}

// Jitter, quantiles, timeouts, continuity, and undefined populations.
func TestLatencyDefinitionFixtures(t *testing.T) {
	t.Parallel()
	ms := func(n int) time.Duration { return time.Duration(n) * time.Millisecond }

	var mixed latencyStats
	for _, rtt := range []int{30, 10, 40, 20} {
		mixed.add(ms(rtt), false, 0)
	}
	mixed.add(0, true, 0)
	mixed.add(0, true, 0)
	mixed.add(0, false, 0) // A non-positive reply is neither an RTT nor a timeout.
	got := mixed.snapshot()
	if got.Count != 4 || got.Min != ms(10) || got.Mean != ms(25) || got.P50 != ms(25) || got.P95 != ms(40) || got.Jitter != ms(70)/3 || timeoutRatio(t, got) != 2.0/6.0 {
		t.Fatalf("mixed fixture: %+v", got)
	}

	var alternating latencyStats
	for _, rtt := range []int{10, 100, 10, 100} {
		alternating.add(ms(rtt), false, 0)
	}
	got = alternating.snapshot()
	if got.Jitter != ms(90) || got.JitterPairs != 3 || got.P50 != ms(55) || got.P10 != ms(10) || got.P90 != ms(100) {
		t.Fatalf("alternating fixture: %+v", got)
	}
	// A snapshot must not sort the receive-order population later replies extend.
	alternating.add(ms(10), false, 0)
	if alternating.snapshot().Jitter != ms(90) {
		t.Fatal("snapshot changed receive order")
	}

	var gaps latencyStats
	gaps.add(ms(10), false, 0)
	gaps.add(0, true, 0)
	gaps.add(ms(20), false, 0)
	gaps.breakContinuity()
	gaps.add(ms(100), false, 0)
	gaps.add(ms(110), false, 0)
	if got := gaps.snapshot(); got.Jitter != ms(10) || got.JitterPairs != 2 {
		t.Fatalf("continuity fixture: %+v", got)
	}

	var timeouts latencyStats
	timeouts.add(0, true, 0)
	if got := timeouts.snapshot(); got.Count != 0 || got.P50 != 0 || got.Mean != 0 || timeoutRatio(t, got) != 1 {
		t.Fatalf("timeout-only fixture: %+v", got)
	}
	if got := (&latencyStats{}).snapshot(); got != (LatencyStats{}) {
		t.Fatalf("empty fixture: %+v", got)
	}
	if _, ok := (LatencyStats{Unresolved: 3, SendFailures: 2}).TimeoutRatio(); ok {
		t.Fatal("unresolved probes and local failures became resolved probes")
	}
	var single, steady latencyStats
	single.add(ms(1), false, 0)
	steady.add(ms(1), false, 0)
	steady.add(ms(1), false, 0)
	if single.snapshot().JitterPairs != 0 {
		t.Fatal("one reply manufactured a variation pair")
	}
	if got := steady.snapshot(); got.Jitter != 0 || got.JitterPairs != 1 {
		t.Fatalf("identical replies must establish zero variation: %+v", got)
	}
}
