package goclient

import (
	"sort"
	"time"
)

type rateStats struct {
	samples []float64
	peak    float64
	total   uint64
	elapsed time.Duration
}

func (s *rateStats) add(v float64) {
	if v <= 0 {
		return
	}
	s.samples = append(s.samples, v)
	if v > s.peak {
		s.peak = v
	}
}

func (s *rateStats) setWindow(total uint64, elapsed time.Duration) {
	s.total = total
	s.elapsed = elapsed
}

func (s *rateStats) result(stage string, dir Direction, serverAuth bool) Result {
	mean := 0.0
	if s.elapsed > 0 {
		mean = float64(s.total) / s.elapsed.Seconds()
	}
	return Result{
		Stage:      stage,
		Direction:  dir,
		MeanBps:    mean,
		PeakBps:    s.peak,
		TotalBytes: s.total,
		Samples:    len(s.samples),
		ServerAuth: serverAuth,
		Elapsed:    s.elapsed,
	}
}

type latencyStats struct {
	values []time.Duration
	lost   int
}

func (s *latencyStats) add(rtt time.Duration, lost bool) {
	if lost {
		s.lost++
		return
	}
	if rtt > 0 {
		s.values = append(s.values, rtt)
	}
}

func (s *latencyStats) snapshot() LatencyStats {
	if len(s.values) == 0 {
		total := s.lost
		loss := 0.0
		if total > 0 {
			loss = 1
		}
		return LatencyStats{Loss: loss}
	}
	xs := append([]time.Duration(nil), s.values...)
	sort.Slice(xs, func(i, j int) bool { return xs[i] < xs[j] })
	var sum time.Duration
	for _, v := range xs {
		sum += v
	}
	mean := sum / time.Duration(len(xs))
	var dev time.Duration
	for _, v := range xs {
		if v > mean {
			dev += v - mean
		} else {
			dev += mean - v
		}
	}
	total := len(xs) + s.lost
	loss := 0.0
	if total > 0 {
		loss = float64(s.lost) / float64(total)
	}
	return LatencyStats{
		Min:    xs[0],
		P50:    percentile(xs, 0.50),
		P95:    percentile(xs, 0.95),
		Mean:   mean,
		Jitter: dev / time.Duration(len(xs)),
		Loss:   loss,
		Count:  len(xs),
	}
}

func percentile(xs []time.Duration, p float64) time.Duration {
	if len(xs) == 0 {
		return 0
	}
	if len(xs) == 1 {
		return xs[0]
	}
	pos := p * float64(len(xs)-1)
	lo := int(pos)
	hi := lo + 1
	if hi >= len(xs) {
		return xs[len(xs)-1]
	}
	frac := pos - float64(lo)
	return time.Duration(float64(xs[lo])*(1-frac) + float64(xs[hi])*frac)
}
