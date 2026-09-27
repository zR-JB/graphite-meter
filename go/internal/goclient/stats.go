package goclient

import (
	"maps"
	"math"
	"slices"
	"time"
)

// latencyStats counts each distinct RTT: a fast reply-driven stage repeats values, so memory stays bounded.
type latencyStats struct {
	counts                             map[time.Duration]int
	count                              int
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
	if rtt < 0 {
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
	if s.counts == nil {
		s.counts = map[time.Duration]int{}
	}
	s.counts[rtt]++
	s.count++
	// A diagnostic cannot turn an otherwise valid raw reply into a missing outcome.
	if handlingNanos <= math.MaxInt64 && time.Duration(handlingNanos) <= rtt {
		s.timingCount++
		s.timingRawSum += rtt
		s.handlingSum += time.Duration(handlingNanos)
	}
}

func (s *latencyStats) snapshot() LatencyStats {
	out := LatencyStats{
		Count:        s.count,
		Timeouts:     s.timeouts,
		Unresolved:   s.unresolved,
		SendFailures: s.sendFailures,
		JitterPairs:  s.pairs,
	}
	if s.timingCount > 0 {
		count := time.Duration(s.timingCount)
		out.ReflectorTiming = &ReflectorTimingStats{
			Count:        s.timingCount,
			MeanRawRTT:   s.timingRawSum / count,
			MeanHandling: s.handlingSum / count,
		}
	}
	if s.pairs > 0 {
		out.Jitter = s.variation / time.Duration(s.pairs)
	}
	if s.count == 0 {
		return out
	}
	sorted := slices.Sorted(maps.Keys(s.counts))
	out.P50 = s.nth(sorted, (s.count+1)/2)
	if s.count%2 == 0 {
		out.P50 += (s.nth(sorted, s.count/2+1) - out.P50) / 2
	}
	out.P95 = s.nth(sorted, max(1, int(math.Ceil(0.95*float64(s.count)))))
	return out
}

func (s *latencyStats) nth(sorted []time.Duration, rank int) time.Duration {
	for _, rtt := range sorted {
		if rank -= s.counts[rtt]; rank <= 0 {
			return rtt
		}
	}
	return sorted[len(sorted)-1]
}
