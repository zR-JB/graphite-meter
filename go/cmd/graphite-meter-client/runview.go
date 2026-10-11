package main

import (
	"cmp"
	"context"
	"errors"
	"fmt"
	"slices"
	"strconv"
	"strings"
	"time"

	"charm.land/lipgloss/v2"
	"github.com/zR-JB/graphite-meter/go/internal/goclient"
)

// A console narrower than dialMin shows the dial's readout without its ring, and puts the cards before the lanes.
const consoleGap, dialMin = 4, 96

// consoleView is a run as the browser's console: the dial beside the latency lanes, the key and the stage track,
// over a card per stage.
func (m model) consoleView(w, h int) string {
	stripH := 4
	if h < 31 {
		stripH = 2
	}
	cards := m.cards(w, stripH)
	if w < dialMin {
		lines := append(m.st.readout(m.dialSpec()), "")
		lines = append(append(lines, m.st.chips(m.chipRow(), w)...), "", m.runKey(w)[1], "")
		lines = append(lines, cards...)
		if lanes := m.lanesCard(w); lanes != nil {
			lines = append(append(lines, ""), lanes...)
		}
		return strings.Join(lines, "\n")
	}
	dialW := min(48, w*2/5)
	cw := w - dialW - consoleGap
	var controls []string
	if lanes := m.lanesCard(cw); lanes != nil {
		controls = append(lanes, "")
	}
	controls = append(append(append(controls, m.runKey(cw)...), ""), m.st.chips(m.chipRow(), cw)...)
	dial := m.st.dial(m.dialSpec(), dialW, max(len(controls), min(h-len(cards)-1, 19)))
	top := lipgloss.JoinHorizontal(lipgloss.Top, strings.Join(dial, "\n"), strings.Repeat(" ", consoleGap),
		strings.Join(controls, "\n"))
	return top + "\n\n" + strings.Join(cards, "\n")
}

// lanesCard is the latency lanes as a card, or nil when the run measures no latency.
func (m model) lanesCard(w int) []string {
	rows := m.laneRows()
	if len(rows) == 0 {
		return nil
	}
	lanes := m.st.lanes(rows, w)
	if m.multipleRunServers() {
		lanes = append([]string{m.st.muted.Render("To " + m.serverName(m.run.latencyServer()) + ", l switches server")}, lanes...)
	}
	return m.st.card(goclient.StageLatency, lanes, w)
}

// runKey is Stop while the run goes, then Run again.
func (m model) runKey(w int) []string {
	if m.running() {
		return m.st.key("Stop", "", "esc", w, true)
	}
	return m.st.key("Run again", m.planTime(), "enter", w, true)
}

// planTime is how long the configured stages take with their warmups.
func (m model) planTime() string {
	plan := m.cfg.Plan()
	total := time.Duration(len(plan)) * m.cfg.Warmup
	for _, stage := range plan {
		total += stage.Duration
	}
	return "about " + fmtSetting(total.Round(time.Second))
}

func (m model) chipRow() []chip {
	var chips []chip
	for _, s := range m.run.stages {
		c := chip{stage: s.name, progress: 1}
		switch s.state {
		case stageMeasuring:
			elapsed := min(max(m.now.Sub(s.since), 0), s.duration)
			c.progress, c.status = elapsed.Seconds()/s.duration.Seconds(), m.st.text.Render(fmtClock(elapsed))
		case stagePreparing, stageWarmup:
			c.progress, c.status = 0, m.spin.View()
		case stageDone:
			c.status = m.st.ok.Render("✓")
		case stagePartial:
			c.status = m.st.warn.Render("! " + stageStatusLabels[s.state])
		case stageFailed:
			c.progress, c.status = 0, m.st.err.Render("✗ "+stageStatusLabels[s.state])
		case stageStopped:
			c.progress, c.status = 0, m.st.muted.Render(stageStatusLabels[s.state])
		default:
			c.progress, c.status = 0, m.st.muted.Render(fmtSetting(s.duration))
			if !m.running() {
				c.status = m.st.muted.Render(stageStatusLabels[s.state])
			}
		}
		chips = append(chips, c)
	}
	return chips
}

// laneRows is a lane for the idle latency and, with loaded latency on, each transfer stage.
func (m model) laneRows() []lane {
	r := m.run
	populations := r.latencyPopulations()
	var rows []lane
	for i, stage := range r.plan {
		if stage.Name != goclient.StageLatency && (!m.cfg.LoadedLatency || len(stage.Directions) == 0) {
			continue
		}
		row := lane{stage: stage.Name}
		population, ok := populations[stage.Name]
		switch state := r.stages[i].state; {
		case ok && population.HasMedian():
			stats := population.Latency
			row.stats = &stats
		case state == stageMeasuring:
			row.note = m.st.muted.Render("measuring")
			if sample, ok := r.latest[r.latencyServer()]; ok {
				row.note += m.st.muted.Render(", now " + fmtMs(sample.RTT))
			}
			if streak := m.timeoutStreak(); streak != "" {
				row.note += "  " + streak
			}
		case r.running() && state == stagePending:
			row.note = m.st.muted.Render("waiting")
		case r.running():
			row.note = m.st.muted.Render("next")
		default:
			row.note = m.st.muted.Render(r.unmeasured(i))
		}
		rows = append(rows, row)
	}
	return rows
}

// dialSpec is what the dial shows: the live stage while one measures, the headline result once the run ends.
func (m model) dialSpec() dial {
	r := m.run
	// Before a rate arrives the dial reads against the browser's 100 Mbit/s reference.
	peak := cmp.Or(r.peak, 12.5e6)
	rateDiv, rateUnit := rateTier(peak*8, 1.2)
	rateDiv = peak * 8 / rateDiv
	gauge := gaugeCeiling(peak)
	rateTicks, msTicks := [5]string{}, [5]string{}
	scale := max(r.rttScale, time.Millisecond)
	for i := range 5 {
		f := float64(i) / 4
		rateTicks[i] = gaugeTick(gaugeValue(f, gauge) * 8 / rateDiv)
		msTicks[i] = gaugeTick(f * float64(scale) / 1e6)
	}
	rate := func(stage goclient.Stage, bytesPerSec float64) dial {
		return dial{arcs: []arc{{gaugeFraction(bytesPerSec, gauge), m.st.trace[stage]}}, ticks: rateTicks,
			label: stageIcons[stage] + " " + stageLabels[stage], value: fmtSpeed(bytesPerSec * 8 / rateDiv),
			unit: rateUnit, hue: m.st.stage[stage]}
	}
	latency := func(rtt time.Duration) dial {
		return dial{arcs: []arc{{float64(rtt) / float64(scale), m.st.trace[goclient.StageLatency]}}, ticks: msTicks,
			label: stageIcons[goclient.StageLatency] + " " + stageLabels[goclient.StageLatency],
			value: strings.TrimSuffix(fmtMs(rtt), " ms"), unit: "ms", hue: m.st.stage[goclient.StageLatency]}
	}
	at := slices.IndexFunc(r.plan, func(s goclient.StagePlan) bool { return s.Name == r.stage })
	if r.running() {
		idle := dial{ticks: rateTicks, label: m.statusLabel(), value: missing, hue: m.st.muted}
		switch {
		case at < 0 || r.phase != goclient.PhaseMeasuring:
			if at >= 0 && len(r.plan[at].Directions) == 0 {
				idle.ticks = msTicks
			}
			return idle
		case len(r.plan[at].Directions) == 0:
			idle.ticks = msTicks
			if sample, ok := r.latest[r.latencyServer()]; ok {
				idle = latency(sample.RTT)
			}
			idle.note = m.timeoutStreak()
			return idle
		}
		total := 0.0
		for _, dir := range r.plan[at].Directions {
			total += r.shown[dir].value(m.now)
		}
		return rate(r.stage, total)
	}
	// Finished: every transfer stage's arc, the first one's figure.
	var head *dial
	var arcs []arc
	for _, stage := range r.plan {
		if mean := r.mean(stage.Name); len(stage.Directions) > 0 && mean > 0 {
			d := rate(stage.Name, mean)
			arcs = append(arcs, d.arcs...)
			if head == nil {
				head = &d
			}
		}
	}
	if head != nil {
		head.arcs = arcs
		return *head
	}
	if p, ok := r.latencyPopulations()[goclient.StageLatency]; ok && p.HasMedian() {
		return latency(p.Latency.P50)
	}
	return dial{ticks: rateTicks, label: outcomeLabels[r.outcome], value: missing, hue: m.st.muted}
}

// timeoutStreak names unanswered probes in a row, in the warning tone and from three on the error tone.
func (m model) timeoutStreak() string {
	streak := m.run.timeouts[m.run.latencyServer()]
	if streak == 0 {
		return ""
	}
	style := m.st.warn
	if streak >= 3 {
		style = m.st.err
	}
	return style.Render(fmt.Sprintf("probe timeout ×%d", streak))
}

// cards lays out a card per stage, as many to a row as keep each one 28 columns wide.
func (m model) cards(w, stripH int) []string {
	plan := m.run.plan
	per := len(plan)
	for per > 1 && (w-2*(per-1))/per < 28 {
		per = (per + 1) / 2
	}
	cw := (w - 2*(per-1)) / max(per, 1)
	var out []string
	for row := 0; row < len(plan); row += per {
		var cards []string
		for i := row; i < min(row+per, len(plan)); i++ {
			lines := m.card(i, cw, stripH)
			for j, line := range lines {
				lines[j] = pad(line, cw)
			}
			cards = append(cards, strings.Join(lines, "\n"))
		}
		if row > 0 {
			out = append(out, "")
		}
		out = append(out, strings.Split(lipgloss.JoinHorizontal(lipgloss.Top, intersperse(cards, "  ")...), "\n")...)
	}
	return out
}

func intersperse(items []string, gap string) []string {
	out := make([]string, 0, 2*len(items))
	for i, item := range items {
		if i > 0 {
			out = append(out, gap)
		}
		out = append(out, item)
	}
	return out
}

// card is one stage: its figure and state, its strip over its window, and its facts. Its height never changes, so
// nothing moves between Start and the result.
func (m model) card(i, w, stripH int) []string {
	r, st := m.run, m.st
	stage, progress := r.plan[i], r.stages[i]
	t0, t1, started := r.window(stage.Name)
	live := r.running() && r.stage == stage.Name && r.phase == goclient.PhaseMeasuring
	figure := func(text string) string {
		at := strings.LastIndex(text, " ")
		return st.value.Render(text[:at]) + " " + st.muted.Render(text[at+1:])
	}
	value, sub := st.muted.Render(missing), ""
	var strip []string
	var facts [][2]string
	var failure error
	population, measured := r.latencyPopulations()[stage.Name]
	measured = measured && population.HasMedian()
	if len(stage.Directions) == 0 {
		failure = population.Err
		facts = [][2]string{{"95th percentile", missing}, {"Replies", missing}, {"Timeouts", missing}}
		switch sample, sampled := r.latest[r.latencyServer()]; {
		case measured:
			s := population.Latency
			value = figure(fmtMs(s.P50))
			if s.JitterPairs > 0 {
				sub = st.muted.Render("jitter " + fmtMs(s.Jitter))
			}
			facts[0][1], facts[1][1] = fmtMs(s.P95), fmtCount(s.Count)
			if ratio, ok := s.TimeoutRatio(); ok {
				facts[2][1] = strconv.FormatFloat(ratio*100, 'f', 1, 64) + "%"
			}
		case live && sampled:
			value = figure(fmtMs(sample.RTT))
		}
		points := r.rtt[r.latencyServer()].points
		if !started {
			points, t1 = nil, 1
		}
		strip = st.line(between(points, t0, t1), float64(max(r.rttScale, time.Millisecond)), t0, t1, w, stripH,
			st.trace[stage.Name])
	} else {
		var bands [][]point
		var rates []string
		total, elapsed, peak := map[goclient.Direction]uint64{}, time.Duration(0), 0.0
		for _, dir := range stage.Directions {
			if started {
				bands = append(bands, between(r.history[dir].points, t0, t1))
			}
			at := slices.IndexFunc(r.results, func(res goclient.Result) bool { return res.Stage == stage.Name && res.Direction == dir })
			switch {
			case at >= 0 && !r.results[at].Unavailable:
				res := r.results[at]
				rates = append(rates, figure(fmtRate(res.MeanBps)))
				total[dir], elapsed, peak = res.TotalBytes, max(elapsed, res.Elapsed), max(peak, res.PeakBps)
			case live && r.shown[dir].to > 0:
				rates = append(rates, figure(fmtRate(r.shown[dir].value(m.now))))
			default:
				rates = append(rates, st.muted.Render(missing))
			}
			if at >= 0 && failure == nil {
				failure = r.results[at].Err
			}
		}
		value = rates[0]
		if len(rates) > 1 {
			value = st.stage[stage.Name].Render("↓ ") + rates[0] + "  " + st.stage[stage.Name].Render("↑ ") + rates[1]
		}
		if !started {
			t1 = 1
		}
		strip = st.strip(stage.Name, bands, r.stripTop(), t0, t1, w, stripH)
		facts = [][2]string{{"Peak", missing}, {"Transferred", missing}, {"Duration", missing}}
		if len(stage.Directions) > 1 {
			facts = [][2]string{{"↓ Transferred", missing}, {"↑ Transferred", missing}, {"Duration", missing}}
		}
		if elapsed > 0 {
			if peak > 0 && len(stage.Directions) == 1 {
				facts[0][1] = fmtRate(peak)
			}
			for j, dir := range stage.Directions {
				facts[j+2-len(stage.Directions)][1] = fmtBytes(total[dir])
			}
			facts[2][1] = fmtClock(elapsed)
		}
		if measured && progress.state == stageDone {
			sub = st.muted.Render("loaded latency " + fmtMs(population.Latency.P50))
		}
	}
	stopped := progress.state == stageStopped || errors.Is(failure, context.Canceled) && r.outcome == goclient.OutcomeStopped
	switch {
	case failure != nil && !errors.Is(failure, context.Canceled), progress.state == stageFailed,
		progress.state == stagePartial:
		reason := stageStatusLabels[progress.state]
		if failure != nil {
			reason = failureLabels[r.failureReason(stage.Name, failure)]
		}
		style := st.err
		if progress.state == stagePartial {
			style = st.warn
		}
		sub = style.Render(reason)
	case stopped:
		sub = st.warn.Render(stageStatusLabels[stageStopped])
	case progress.state == stagePreparing, progress.state == stageWarmup:
		sub = m.spin.View() + st.muted.Render(" "+m.statusLabel())
	case progress.state == stageMeasuring:
		sub = st.muted.Render("measuring")
	case progress.state == stagePending && r.running():
		sub = st.muted.Render(fmtSetting(progress.duration))
	case progress.state == stagePending:
		sub = st.muted.Render(stageStatusLabels[progress.state])
	}
	// A short strip goes without the spacing around it.
	spacer := []string{""}
	if stripH < 4 {
		spacer = nil
	}
	body := append(append([]string{value, sub}, spacer...), strip...)
	body = append(body, spacer...)
	for _, f := range facts {
		body = append(body, st.fact(f[0], f[1], w))
	}
	return st.card(stage.Name, body, w)
}

// between is the points from t0 up to t1.
func between(points []point, t0, t1 float64) []point {
	from, _ := slices.BinarySearchFunc(points, t0, func(p point, t float64) int { return cmp.Compare(p.t, t) })
	to, _ := slices.BinarySearchFunc(points, t1, func(p point, t float64) int { return cmp.Compare(p.t, t) })
	return points[from:to]
}
