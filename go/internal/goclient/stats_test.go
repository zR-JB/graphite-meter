package goclient

import (
	"testing"
	"time"
)

func TestPercentile(t *testing.T) {
	ms := func(n int64) time.Duration { return time.Duration(n) * time.Millisecond }

	t.Run("empty", func(t *testing.T) {
		if got := percentile(nil, 0.5); got != 0 {
			t.Errorf("percentile(nil, 0.5) = %v, want 0", got)
		}
	})

	t.Run("single sample regardless of p", func(t *testing.T) {
		xs := []time.Duration{ms(42)}
		for _, p := range []float64{0, 0.5, 0.95, 1.0} {
			if got := percentile(xs, p); got != ms(42) {
				t.Errorf("percentile(single, %v) = %v, want %v", p, got, ms(42))
			}
		}
	})

	t.Run("boundaries", func(t *testing.T) {
		xs := []time.Duration{ms(10), ms(20), ms(30), ms(40), ms(50)}
		if got := percentile(xs, 0); got != ms(10) {
			t.Errorf("p=0 = %v, want min %v", got, ms(10))
		}
		if got := percentile(xs, 1.0); got != ms(50) {
			t.Errorf("p=1.0 = %v, want max %v", got, ms(50))
		}
	})

	t.Run("midpoint median", func(t *testing.T) {
		xs := []time.Duration{ms(10), ms(20), ms(30), ms(40)}
		want := ms(25)
		if got := median(xs); got != want {
			t.Errorf("median(4 samples) = %v, want %v", got, want)
		}
	})
}

func TestRateStatsResult(t *testing.T) {
	t.Run("zero samples", func(t *testing.T) {
		var s rateStats
		r := s.result("download", Down, true)
		if r.MeanBps != 0 {
			t.Errorf("MeanBps = %v, want 0", r.MeanBps)
		}
		if r.Samples != 0 {
			t.Errorf("Samples = %v, want 0", r.Samples)
		}
		if r.PeakBps != 0 {
			t.Errorf("PeakBps = %v, want 0", r.PeakBps)
		}
	})

	t.Run("with samples", func(t *testing.T) {
		var s rateStats
		s.add(10)
		s.add(0)
		s.add(-5)
		s.add(30)
		s.add(20)
		s.setWindow(300, 2*time.Second)
		r := s.result("upload", Up, false)
		if r.Stage != "upload" {
			t.Errorf("Stage = %q, want upload", r.Stage)
		}
		if r.Direction != Up {
			t.Errorf("Direction = %v, want Up", r.Direction)
		}
		if r.ServerAuth != false {
			t.Errorf("ServerAuth = %v, want false", r.ServerAuth)
		}
		if r.Elapsed != 2*time.Second {
			t.Errorf("Elapsed = %v, want 2s", r.Elapsed)
		}
		if r.MeanBps != 150 {
			t.Errorf("MeanBps = %v, want 150", r.MeanBps)
		}
		if r.PeakBps != 30 {
			t.Errorf("PeakBps = %v, want 30", r.PeakBps)
		}
		if r.TotalBytes != 300 {
			t.Errorf("TotalBytes = %v, want 300", r.TotalBytes)
		}
		if r.Samples != 3 {
			t.Errorf("Samples = %v, want 3", r.Samples)
		}
	})

}

func TestLatencyStatsAdd(t *testing.T) {
	var s latencyStats
	s.add(10*time.Millisecond, false, 0)
	s.add(0, true, 0)  // lost: counted, value not recorded
	s.add(0, false, 0) // not lost but non-positive: skipped from values
	s.add(20*time.Millisecond, false, 0)

	if s.timeouts != 1 {
		t.Errorf("lost = %d, want 1", s.timeouts)
	}
	if len(s.values) != 2 {
		t.Errorf("values = %v, want 2 entries", s.values)
	}
}

func TestLatencyStatsSnapshot(t *testing.T) {
	t.Run("zero samples", func(t *testing.T) {
		var s latencyStats
		got := s.snapshot()
		want := LatencyStats{}
		if got != want {
			t.Errorf("snapshot() = %+v, want %+v", got, want)
		}
	})

	t.Run("all lost", func(t *testing.T) {
		var s latencyStats
		s.add(0, true, 0)
		s.add(0, true, 0)
		s.add(0, true, 0)
		got := s.snapshot()
		if timeoutRatio(t, got) != 1 {
			t.Errorf("Loss = %v, want 1", timeoutRatio(t, got))
		}
		if got.Count != 0 {
			t.Errorf("Count = %v, want 0", got.Count)
		}
		if got.P50 != 0 || got.P95 != 0 || got.Mean != 0 || got.Min != 0 {
			t.Errorf("expected zero-value latency fields, got %+v", got)
		}
	})

	t.Run("mixed", func(t *testing.T) {
		var s latencyStats
		// unsorted insertion order; snapshot must sort internally
		s.add(30*time.Millisecond, false, 0)
		s.add(10*time.Millisecond, false, 0)
		s.add(40*time.Millisecond, false, 0)
		s.add(20*time.Millisecond, false, 0)
		s.add(0, true, 0)
		s.add(0, true, 0)

		got := s.snapshot()

		if got.Min != 10*time.Millisecond {
			t.Errorf("Min = %v, want 10ms", got.Min)
		}
		if got.Mean != 25*time.Millisecond {
			t.Errorf("Mean = %v, want 25ms", got.Mean)
		}
		if got.Jitter != 70*time.Millisecond/3 {
			t.Errorf("Jitter = %v, want 70ms/3 (receive-order variation)", got.Jitter)
		}
		if got.P50 != 25*time.Millisecond {
			t.Errorf("P50 = %v, want 25ms", got.P50)
		}
		if got.P95 != 40*time.Millisecond {
			t.Errorf("P95 = %v, want 40ms", got.P95)
		}
		if got.Count != 4 {
			t.Errorf("Count = %v, want 4", got.Count)
		}
		wantLoss := 2.0 / 6.0 // 2 lost out of 4 values + 2 lost
		if timeoutRatio(t, got) != wantLoss {
			t.Errorf("Loss = %v, want %v", timeoutRatio(t, got), wantLoss)
		}
	})
}

var benchmarkResult Result

func BenchmarkMeasurementReduction(b *testing.B) {
	b.ReportAllocs()
	for b.Loop() {
		var rates rateStats
		for sample := range 100 {
			rates.add(float64(100_000_000 + sample))
		}
		rates.setWindow(1_000_000_000, 10*time.Second)
		benchmarkResult = rates.result("download", Down, false)
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

func TestLatencyDefinitionFixtures(t *testing.T) {
	var s latencyStats
	for _, ms := range []int{10, 100, 10, 100} {
		s.add(time.Duration(ms)*time.Millisecond, false, 0)
	}
	got := s.snapshot()
	if got.Jitter != 90*time.Millisecond || got.JitterPairs != 3 || got.P50 != 55*time.Millisecond || got.P10 != 10*time.Millisecond || got.P90 != 100*time.Millisecond || got.P95 != 100*time.Millisecond {
		t.Fatalf("alternating fixture: %+v", got)
	}
	// Taking a snapshot must not sort the receive-order population used by later replies.
	s.add(10*time.Millisecond, false, 0)
	if s.snapshot().Jitter != 90*time.Millisecond {
		t.Fatal("snapshot changed receive order")
	}
	var gaps latencyStats
	gaps.add(10*time.Millisecond, false, 0)
	gaps.add(0, true, 0)
	gaps.add(20*time.Millisecond, false, 0)
	gaps.breakContinuity()
	gaps.add(100*time.Millisecond, false, 0)
	gaps.add(110*time.Millisecond, false, 0)
	got = gaps.snapshot()
	if got.Jitter != 10*time.Millisecond || got.JitterPairs != 2 {
		t.Fatalf("continuity fixture: %+v", got)
	}
}

func TestLatencyMissingPopulations(t *testing.T) {
	if _, ok := (LatencyStats{}).TimeoutRatio(); ok {
		t.Fatal("empty population has a timeout ratio")
	}
	if _, ok := (LatencyStats{Unresolved: 3, SendFailures: 2}).TimeoutRatio(); ok {
		t.Fatal("unresolved/local failures became resolved probes")
	}
	if got := timeoutRatio(t, LatencyStats{Timeouts: 3, Unresolved: 2}); got != 1 {
		t.Fatalf("timeout-only ratio = %v", got)
	}
	var single latencyStats
	single.add(time.Millisecond, false, 0)
	if single.snapshot().JitterPairs != 0 {
		t.Fatal("one reply manufactured a variation pair")
	}
	var steady latencyStats
	steady.add(time.Millisecond, false, 0)
	steady.add(time.Millisecond, false, 0)
	if got := steady.snapshot(); got.Jitter != 0 || got.JitterPairs != 1 {
		t.Fatalf("valid zero variation: %+v", got)
	}
}

func TestNearestRankPercentiles(t *testing.T) {
	xs := []time.Duration{10, 20, 30, 40}
	for _, tt := range []struct {
		p    float64
		want time.Duration
	}{{0.1, 10}, {0.9, 40}, {0.95, 40}} {
		if got := percentile(xs, tt.p); got != tt.want {
			t.Fatalf("p=%v: %v, want %v", tt.p, got, tt.want)
		}
	}
}
