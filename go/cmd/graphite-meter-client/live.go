package main

import (
	"math"
	"slices"
	"strconv"
	"time"

	"github.com/zR-JB/graphite-meter/go/internal/goclient"
)

// What the console reads of a run: each stage's state and time, the latency server's populations and probes, the
// needle and the result's sweep.

// needleTau is how fast the needle closes on its reading: by e⁻¹ every 110 ms, so it rises, falls and drains without
// a jump.
const needleTau = 0.11

// populationKey names one server's replies in one stage.
type populationKey struct {
	stage  goclient.Stage
	server string
}

// needleAt is the needle at now on its way to `to`, 0–1 of the sweep.
func (r *runState) needleAt(to float64, now time.Time) float64 {
	to = min(max(to, 0), 1)
	shown := to
	if !r.needleWhen.IsZero() {
		shown = to + (r.needle-to)*math.Exp(-now.Sub(r.needleWhen).Seconds()/needleTau)
	}
	r.needle, r.needleWhen = shown, now
	return shown
}

// swept is a result arc reaching `to`, cut at the front sweeping on from where the needle stood: in that share of
// 0.9 s and no less than 0.36 s, easing out.
func (r *runState) swept(to float64, now time.Time) float64 {
	if r.finished.IsZero() {
		return to
	}
	over := max(0.9*(1-r.sweepFrom), 0.36)
	share := min(now.Sub(r.finished).Seconds()/over, 1)
	return min(to, r.sweepFrom+(1-r.sweepFrom)*(1-math.Pow(1-share, 3)))
}

// moving is whether the needle or the result's sweep still moves.
func (r *runState) moving(now time.Time) bool {
	if !r.finished.IsZero() {
		return now.Sub(r.finished) < time.Second
	}
	return r.needle > 1e-3
}

// directions are the directions stage moves.
func (r *runState) directions(stage goclient.Stage) []goclient.Direction {
	i := slices.IndexFunc(r.plan, func(s goclient.StagePlan) bool { return s.Name == stage })
	if i < 0 {
		return nil
	}
	return r.plan[i].Directions
}

// plan is the stages shown: the run's plan once it started, else the settings'.
func (m model) plan() []goclient.StagePlan {
	switch {
	case m.run != nil && len(m.run.plan) > 0:
		return m.run.plan
	case m.next != nil:
		return m.next.plan
	}
	return m.cfg.Plan()
}

// plannedDuration is how long stage measures: in the run while one goes, else in the settings.
func (m model) plannedDuration(stage goclient.Stage) (time.Duration, bool) {
	plan := m.cfg.Plan()
	if m.running() {
		plan = m.plan()
	}
	for _, s := range plan {
		if s.Name == stage {
			return s.Duration, true
		}
	}
	return 0, false
}

func (m model) stageState(stage goclient.Stage) stageState {
	if m.run == nil {
		return stagePending
	}
	if i := slices.IndexFunc(m.run.stages, func(s stageProgress) bool { return s.name == stage }); i >= 0 {
		return m.run.stages[i].state
	}
	return stagePending
}

// elapsed is how long stage's warmup or window has run.
func (m model) elapsed(stage goclient.Stage) time.Duration {
	if m.run == nil {
		return 0
	}
	if i := slices.IndexFunc(m.run.stages, func(s stageProgress) bool { return s.name == stage }); i >= 0 {
		return max(m.now.Sub(m.run.stages[i].since), 0)
	}
	return 0
}

// reached is the share of its planned time a stopped stage measured.
func (m model) reached(stage goclient.Stage) float64 {
	planned, ok := m.plannedDuration(stage)
	if !ok || planned <= 0 {
		return 0
	}
	for _, result := range m.runResults() {
		if result.Stage == stage {
			return result.Elapsed.Seconds() / planned.Seconds()
		}
	}
	return 0
}

// population is the latency server's statistics in stage: the stage's own once it ended, else its replies so far.
func (m model) population(stage goclient.Stage) *goclient.LatencyStats {
	r := m.run
	if r == nil {
		return nil
	}
	if result, ok := r.latencyPopulations()[stage]; ok {
		if !result.HasMedian() {
			return nil
		}
		return &result.Latency
	}
	if live, ok := r.populations[populationKey{stage, r.latencyServer()}]; ok {
		stats := live.Stats()
		return &stats
	}
	return nil
}

// latest is the latency server's latest round trip.
func (m model) latest() (time.Duration, bool) {
	if m.run == nil {
		return 0, false
	}
	sample, ok := m.run.latest[m.run.latencyServer()]
	return sample.RTT, ok
}

// timeoutStreak is the unanswered probes in a row, in the warning tone and from three on the error tone.
func (m model) timeoutStreak() line {
	if m.run == nil {
		return nil
	}
	streak := m.run.timeouts[m.run.latencyServer()]
	if streak == 0 {
		return nil
	}
	st := m.pal.warn
	if streak >= 3 {
		st = m.pal.err
	}
	noun := "probes"
	if streak == 1 {
		noun = "probe"
	}
	return styled(strconv.Itoa(streak)+" "+noun+" unanswered", st)
}

// peak is the highest combined rate so far, which sets the unit and the scales.
func (m model) peak() float64 {
	if m.run == nil {
		return 0
	}
	return m.run.peak
}

func (m model) runResults() []goclient.Result {
	if m.run == nil {
		return nil
	}
	return m.run.results
}

// left is the time left in the measuring stage's window, for the status bar.
func (m model) left() (time.Duration, bool) {
	if !m.running() || m.run == nil || m.run.phase != goclient.PhaseMeasuring {
		return 0, false
	}
	planned, ok := m.plannedDuration(m.run.stage)
	return max(planned-m.elapsed(m.run.stage), 0), ok
}
