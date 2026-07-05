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

	t.Run("interpolated fraction", func(t *testing.T) {
		// 4 samples: pos = 0.5*3 = 1.5 -> lo=1, hi=2, frac=0.5
		// interpolate xs[1]=20ms and xs[2]=30ms -> 25ms
		xs := []time.Duration{ms(10), ms(20), ms(30), ms(40)}
		want := ms(25)
		if got := percentile(xs, 0.5); got != want {
			t.Errorf("percentile(4 samples, 0.5) = %v, want %v", got, want)
		}
	})
}

func TestRateStatsAdd(t *testing.T) {
	t.Run("ignores non-positive values", func(t *testing.T) {
		var s rateStats
		s.add(0, 10)
		s.add(-5, 10)
		if len(s.samples) != 0 {
			t.Errorf("samples = %v, want empty", s.samples)
		}
		if s.peak != 0 {
			t.Errorf("peak = %v, want 0", s.peak)
		}
		if s.total != 0 {
			t.Errorf("total = %v, want 0", s.total)
		}
	})

	t.Run("tracks running peak", func(t *testing.T) {
		var s rateStats
		s.add(5, 1)
		s.add(3, 2)
		s.add(10, 3)
		s.add(1, 4)
		if s.peak != 10 {
			t.Errorf("peak = %v, want 10", s.peak)
		}
		if len(s.samples) != 4 {
			t.Errorf("samples len = %d, want 4", len(s.samples))
		}
	})

	t.Run("total is monotonically increasing", func(t *testing.T) {
		var s rateStats
		s.add(1, 100)
		s.add(1, 50) // lower total must not decrease running total
		if s.total != 100 {
			t.Errorf("total = %v, want 100", s.total)
		}
		s.add(1, 200)
		if s.total != 200 {
			t.Errorf("total = %v, want 200", s.total)
		}
	})
}

func TestRateStatsResult(t *testing.T) {
	t.Run("zero samples", func(t *testing.T) {
		var s rateStats
		r := s.result("download", Down, true, 5*time.Second)
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
		s.add(10, 100)
		s.add(20, 200)
		s.add(30, 300)
		r := s.result("upload", Up, false, 2*time.Second)
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
		if r.MeanBps != 20 {
			t.Errorf("MeanBps = %v, want 20", r.MeanBps)
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
	s.add(10*time.Millisecond, false)
	s.add(0, true)  // lost: counted, value not recorded
	s.add(0, false) // not lost but non-positive: skipped from values
	s.add(20*time.Millisecond, false)

	if s.lost != 1 {
		t.Errorf("lost = %d, want 1", s.lost)
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
		s.add(0, true)
		s.add(0, true)
		s.add(0, true)
		got := s.snapshot()
		if got.Loss != 1 {
			t.Errorf("Loss = %v, want 1", got.Loss)
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
		s.add(30*time.Millisecond, false)
		s.add(10*time.Millisecond, false)
		s.add(40*time.Millisecond, false)
		s.add(20*time.Millisecond, false)
		s.add(0, true)
		s.add(0, true)

		got := s.snapshot()

		sorted := []time.Duration{10 * time.Millisecond, 20 * time.Millisecond, 30 * time.Millisecond, 40 * time.Millisecond}
		wantP50 := percentile(sorted, 0.50)
		wantP95 := percentile(sorted, 0.95)

		if got.Min != 10*time.Millisecond {
			t.Errorf("Min = %v, want 10ms", got.Min)
		}
		if got.Mean != 25*time.Millisecond {
			t.Errorf("Mean = %v, want 25ms", got.Mean)
		}
		if got.Jitter != 10*time.Millisecond {
			t.Errorf("Jitter = %v, want 10ms (mean absolute deviation)", got.Jitter)
		}
		if got.P50 != wantP50 {
			t.Errorf("P50 = %v, want %v (via percentile)", got.P50, wantP50)
		}
		if got.P95 != wantP95 {
			t.Errorf("P95 = %v, want %v (via percentile)", got.P95, wantP95)
		}
		if got.Count != 4 {
			t.Errorf("Count = %v, want 4", got.Count)
		}
		wantLoss := 2.0 / 6.0 // 2 lost out of 4 values + 2 lost
		if got.Loss != wantLoss {
			t.Errorf("Loss = %v, want %v", got.Loss, wantLoss)
		}
	})
}
