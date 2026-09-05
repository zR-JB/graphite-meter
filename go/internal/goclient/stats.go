package goclient

import (
	"context"
	"math"
	"slices"
	"sync"
	"time"
)

type laneGroup struct {
	cancel context.CancelFunc
	wg     sync.WaitGroup
	errs   chan error
}

func (r *runner) startLanes(ctx context.Context, streams int, body func(ctx context.Context, lane int) error) *laneGroup {
	laneCtx, cancel := context.WithCancel(ctx)
	g := &laneGroup{cancel: cancel, errs: make(chan error, streams)}
	stagger := r.laneStaggerStep(streams)
	for lane := range streams {
		g.wg.Go(func() {
			if !staggerSleep(laneCtx, lane, stagger) {
				return
			}
			if err := body(laneCtx, lane); err != nil {
				select {
				case g.errs <- err:
				default:
				}
			}
		})
	}
	return g
}

func (g *laneGroup) waitStart(ctx context.Context, start <-chan struct{}) error {
	select {
	case <-ctx.Done():
		return ctx.Err()
	case err := <-g.errs:
		g.stop()
		return err
	case <-start:
		return nil
	}
}

func (g *laneGroup) stop() {
	g.cancel()
	g.wg.Wait()
}

const rateSampleInterval = 100 * time.Millisecond

type rateLoop struct {
	duration         time.Duration
	cancelEndsWindow bool
	laneErr          <-chan error
	stageErr         <-chan error
	sample           func(now time.Time, stats *rateStats)
	window           func(stats *rateStats)
}

func (l rateLoop) run(ctx context.Context) (rateStats, error) {
	ticker := time.Tick(rateSampleInterval)
	var deadline <-chan time.Time
	if l.duration > 0 {
		timer := time.NewTimer(l.duration)
		defer timer.Stop()
		deadline = timer.C
	}
	var stats rateStats
	finish := func(err error, canceled bool) (rateStats, error) {
		l.window(&stats)
		if err == nil || canceled && l.cancelEndsWindow {
			return stats, nil
		}
		return stats, err
	}
	for {
		select {
		case <-ctx.Done():
			return finish(ctx.Err(), true)
		case <-deadline:
			return finish(nil, false)
		case err := <-l.laneErr:
			return finish(err, false)
		case err := <-l.stageErr:
			return finish(err, false)
		case now := <-ticker:
			l.sample(now, &stats)
		}
	}
}

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
	s.peak = max(s.peak, v)
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
	values                             []time.Duration
	timeouts, unresolved, sendFailures int
	previous                           time.Duration
	hasPrevious                        bool
	variation                          time.Duration
	pairs                              int
}

func (s *latencyStats) breakContinuity() { s.hasPrevious = false }

func (s *latencyStats) add(rtt time.Duration, timeout bool) {
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
}

func (s *latencyStats) snapshot() LatencyStats {
	out := LatencyStats{Count: len(s.values), Timeouts: s.timeouts, Unresolved: s.unresolved, SendFailures: s.sendFailures, JitterPairs: s.pairs}
	if s.pairs > 0 {
		out.Jitter = s.variation / time.Duration(s.pairs)
	}
	if len(s.values) == 0 {
		return out
	}
	xs := slices.Clone(s.values)
	slices.Sort(xs)
	var sum time.Duration
	for _, v := range xs {
		sum += v
	}
	out.Min, out.Max, out.Mean = xs[0], xs[len(xs)-1], sum/time.Duration(len(xs))
	out.P10, out.P50 = percentile(xs, 0.10), median(xs)
	out.P90, out.P95 = percentile(xs, 0.90), percentile(xs, 0.95)
	return out
}

// median is the midpoint of the two central observations for an even-sized sorted population.
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

// percentile selects the nearest rank from a sorted observation population.
func percentile(xs []time.Duration, p float64) time.Duration {
	if len(xs) == 0 {
		return 0
	}
	rank := max(1, min(len(xs), int(math.Ceil(p*float64(len(xs))))))
	return xs[rank-1]
}
