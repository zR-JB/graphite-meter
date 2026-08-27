package goclient

import (
	"context"
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
	xs := slices.Clone(s.values)
	slices.Sort(xs)
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
