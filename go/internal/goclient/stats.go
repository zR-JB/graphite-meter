package goclient

import (
	"math"
	"slices"
	"time"
)

type latencyStats struct {
	values                             []time.Duration
	timeouts, unresolved, sendFailures int
	previous                           time.Duration
	hasPrevious                        bool
	variation                          time.Duration
	pairs                              int
	timingCount                        int
	timingRawSum, handlingSum          time.Duration
}

func (s *latencyStats) breakContinuity() { s.hasPrevious = false }

func (s *latencyStats) add(rtt time.Duration, timeout bool, handlingNanos uint64) {
	if timeout {
		s.timeouts++
		return
	}
	if rtt <= 0 {
		return
	}
	if s.hasPrevious {
		delta := rtt - s.previous
		if delta < 0 {
			delta = -delta
		}
		s.variation += delta
		s.pairs++
	}
	s.previous, s.hasPrevious = rtt, true
	s.values = append(s.values, rtt)
	// A diagnostic cannot turn an otherwise valid raw reply into a missing outcome.
	if handlingNanos <= math.MaxInt64 && time.Duration(handlingNanos) <= rtt {
		s.timingCount++
		s.timingRawSum += rtt
		s.handlingSum += time.Duration(handlingNanos)
	}
}

func (s *latencyStats) snapshot() LatencyStats {
	out := LatencyStats{
		Count:        len(s.values),
		Timeouts:     s.timeouts,
		Unresolved:   s.unresolved,
		SendFailures: s.sendFailures,
		JitterPairs:  s.pairs,
	}
	if s.timingCount > 0 {
		count := time.Duration(s.timingCount)
		out.ReflectorTiming = &ReflectorTimingStats{
			Count:           s.timingCount,
			MeanRawRTT:      s.timingRawSum / count,
			MeanHandling:    s.handlingSum / count,
			MeanAdjustedRTT: (s.timingRawSum - s.handlingSum) / count,
		}
	}
	if s.pairs > 0 {
		out.Jitter = s.variation / time.Duration(s.pairs)
	}
	if len(s.values) == 0 {
		return out
	}
	xs := slices.Sorted(slices.Values(s.values))
	out.P50, out.P95 = median(xs), percentile(xs, 0.95)
	return out
}

func median(xs []time.Duration) time.Duration {
	if len(xs) == 0 {
		return 0
	}
	mid := len(xs) / 2
	if len(xs)%2 != 0 {
		return xs[mid]
	}
	return xs[mid-1] + (xs[mid]-xs[mid-1])/2
}

func percentile(xs []time.Duration, p float64) time.Duration {
	if len(xs) == 0 {
		return 0
	}
	rank := max(1, min(len(xs), int(math.Ceil(p*float64(len(xs))))))
	return xs[rank-1]
}
