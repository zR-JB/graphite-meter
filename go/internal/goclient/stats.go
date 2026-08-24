package goclient

import (
	"context"
	"slices"
	"sync"
	"time"
)

// laneGroup runs one direction's per-stream transfer lanes under a context the
// caller can cancel independently of the run.
type laneGroup struct {
	cancel context.CancelFunc
	wg     sync.WaitGroup
	// errs is buffered per lane so a failing lane never blocks on a reader.
	errs chan error
}

// startLanes spawns `streams` lanes of body, staggered so their congestion
// windows don't ramp in lockstep. The count is the direction's own: the two
// differ under an automatic multiplexed policy.
func (r *runner) startLanes(ctx context.Context, streams int, body func(ctx context.Context, lane int) error) *laneGroup {
	laneCtx, cancel := context.WithCancel(ctx)
	g := &laneGroup{cancel: cancel, errs: make(chan error, streams)}
	stagger := r.laneStaggerStep(streams)
	for i := range streams {
		g.wg.Add(1)
		go func(lane int) {
			defer g.wg.Done()
			if !staggerSleep(laneCtx, lane, stagger) {
				return
			}
			if err := body(laneCtx, lane); err != nil {
				select {
				case g.errs <- err:
				default:
				}
			}
		}(i)
	}
	return g
}

// waitStart blocks until the warmup gate opens; a lane failure meanwhile stops
// the group and is reported instead.
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

// stop cancels the lanes and joins them.
func (g *laneGroup) stop() {
	g.cancel()
	g.wg.Wait()
}

const rateSampleInterval = 100 * time.Millisecond

// rateLoop folds a rate source into rateStats on a steady cadence until the
// measurement window closes, a lane fails, or ctx ends.
type rateLoop struct {
	// duration bounds the window with its own timer; zero leaves ctx as the
	// only bound.
	duration time.Duration
	// cancelEndsWindow reports ctx cancellation as the normal end of the
	// measurement, for callers whose ctx carries the window as its deadline.
	cancelEndsWindow bool
	laneErr          <-chan error
	stageErr         <-chan error
	// sample folds one tick of the rate source into stats and emits it.
	sample func(now time.Time, stats *rateStats)
	// window records the measured byte and time totals at the end of the run.
	window func(stats *rateStats)
}

func (l rateLoop) run(ctx context.Context) (rateStats, error) {
	ticker := time.NewTicker(rateSampleInterval)
	defer ticker.Stop()
	var deadline <-chan time.Time
	if l.duration > 0 {
		timer := time.NewTimer(l.duration)
		defer timer.Stop()
		deadline = timer.C
	}
	var stats rateStats
	for {
		select {
		case <-ctx.Done():
			l.window(&stats)
			if l.cancelEndsWindow {
				return stats, nil
			}
			return stats, ctx.Err()
		case <-deadline:
			l.window(&stats)
			return stats, nil
		case err := <-l.laneErr:
			l.window(&stats)
			return stats, err
		case err := <-l.stageErr:
			l.window(&stats)
			return stats, err
		case now := <-ticker.C:
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
