package main

import (
	"fmt"
	"math"
	"slices"
	"strconv"
	"strings"
	"time"

	"github.com/zR-JB/graphite-meter/go/internal/goclient"
)

// The console, the one screen before, during and after a run, as the browser's: the dial beside the latency lanes,
// the run key over the stage chips, and a card per stage across the bottom (docs/DESIGN.md, Terminal).

// wideConsole is from where the dial stands beside the lanes over the cards; below it the console stacks without the
// ring.
var wideConsole = [2]int{110, 32}

// controlsWidth is the run key's and the chips' widest.
const controlsWidth = 64

// gaugeKnots is the browser's dial transfer curve (client/src/lib/components/gaugeScale.ts): equal sweeps for each.
var gaugeKnots = [...]float64{0, 0.005, 0.01, 0.05, 0.1, 0.25, 0.5, 0.75, 1}

// gaugeFraction is where value sits on a dial reaching scale, from 0 to 1.
func gaugeFraction(value, scale float64) float64 {
	v := min(max(value/scale, 0), 1)
	if math.IsNaN(v) {
		return 0
	}
	i := 1
	for i < len(gaugeKnots)-1 && v > gaugeKnots[i] {
		i++
	}
	spans := float64(len(gaugeKnots) - 1)
	return (float64(i) - 1 + (v-gaugeKnots[i-1])/(gaugeKnots[i]-gaugeKnots[i-1])) / spans
}

// gaugeValue is the value at fraction of a dial reaching scale.
func gaugeValue(fraction, scale float64) float64 {
	at := min(max(fraction, 0), 1) * float64(len(gaugeKnots)-1)
	i := min(int(at), len(gaugeKnots)-2)
	return scale * (gaugeKnots[i] + (at-float64(i))*(gaugeKnots[i+1]-gaugeKnots[i]))
}

// ceilStep is the browser's axis ceiling (client/src/lib/presentation/scales.ts): the first step at or above v.
func ceilStep(v float64, steps ...float64) float64 {
	if math.IsNaN(v) || math.IsInf(v, 0) || v <= 0 {
		return steps[0]
	}
	decade := math.Pow(10, math.Floor(math.Log10(v)))
	for _, step := range steps {
		if step*decade >= v {
			return step * decade
		}
	}
	return 10 * decade
}

// gaugeTick is the browser's tick label: bounded precision, no trailing zeros.
func gaugeTick(v float64) string {
	if v == 0 || math.IsNaN(v) {
		return "0"
	}
	places := min(max(2-int(math.Floor(math.Log10(math.Abs(v)))), 0), 6)
	text := strconv.FormatFloat(v, 'f', places, 64)
	if strings.Contains(text, ".") {
		text = strings.TrimRight(strings.TrimRight(text, "0"), ".")
	}
	return text
}

var stageIcons = map[goclient.Stage]string{
	goclient.StageLatency: "≈", goclient.StageDownload: "↓", goclient.StageUpload: "↑", goclient.StageBidirectional: "↕",
}

// gaugeCeiling is the browser's dial scale for a combined rate: its 1-2-5 step above it with 3% headroom, and from a
// megabit up never under 1 Gbit/s, where most connections fit.
func gaugeCeiling(bytesPerSec float64) float64 {
	bits := bytesPerSec * 8 * 1.03
	top := ceilStep(bits, 1, 2, 5)
	if bits >= 1e6 {
		top = max(top, 1e9)
	}
	return top / 8
}

// unit is the page's rate unit, the dial's, from the highest rate so far; before any, the browser's 100 Mbit/s.
type unit struct {
	divisor float64
	name    string
}

func unitOf(peak float64) unit {
	if peak <= 0 {
		peak = 12.5e6
	}
	shown, name := rateTier(peak*8, 1.2)
	return unit{peak * 8 / shown, name}
}

func (u unit) figure(bytesPerSec float64) string { return fmtSpeed(bytesPerSec * 8 / u.divisor) }

// lane is a latency lane: a population's statistics, its latest reply while it is measured, and whether it is.
type lane struct {
	stage   goclient.Stage
	stats   *goclient.LatencyStats
	latest  time.Duration
	hasLast bool
	live    bool
}

// scales are the cards' tops: the rate charts' in bytes per second, and the idle and loaded reply tracks' in ms.
type scales struct{ rate, idle, loaded float64 }

// console draws the console in area: wide, the dial beside the lanes and controls over the cards; narrow, stacked.
func (m model) console(c *canvas, area rect) {
	lanes := m.lanes()
	if area.w >= wideConsole[0] && area.h >= wideConsole[1] {
		top := min(max(area.h*11/20, 18), 24)
		d := rect{area.x + 1, area.y + 1, min(max(area.w*3/10, 46), 66), top}
		m.pal.drawDial(c, d, m.dialSpec())
		side := rect{d.right() + 3, d.y, max(area.right()-d.right()-5, 0), top}
		controls := side
		if len(lanes) > 0 {
			pitch := 1
			if top >= 22 {
				pitch = 2
			}
			height := 4 + pitch*len(lanes)
			m.lanesArea(c, rect{side.x, side.y, side.w, height}, lanes, pitch)
			controls = rect{side.x, side.y + height + 1, side.w, max(side.h-height-1, 0)}
		}
		y := controls.y + max(controls.h-6, 0)/2
		m.controls(c, rect{controls.x, y, controls.w, 6})
		m.cards(c, rect{area.x + 1, d.bottom() + 1, area.w - 2, area.bottom() - d.bottom() - 1})
		return
	}
	// Stacked: the readout, the controls, the cards, and the lanes when room is left; the shortest area drops the gap
	// under the readout.
	readout := rect{area.x, area.y, area.w, 8}
	m.pal.readout(c, readout, m.dialSpec())
	width := min(max(area.w-4, 0), controlsWidth)
	controls := rect{area.x + (area.w-width)/2, readout.bottom() + min(max(area.h-14, 0), 1), width, 6}
	m.controls(c, controls)
	rest := max(area.bottom()-controls.bottom()-1, 0)
	lanesHeight := 4 + len(lanes)
	room := len(lanes) > 0 && rest > 8+lanesHeight
	cardsHeight := rest
	if room {
		cardsHeight = rest - lanesHeight - 1
	}
	cards := rect{area.x + 1, controls.bottom() + 1, area.w - 2, cardsHeight}
	m.cards(c, cards)
	if room {
		m.lanesArea(c, rect{area.x + 1, cards.bottom() + 1, area.w - 2, lanesHeight}, lanes, 1)
	}
}

// controls draws the run key over the stage chips, as wide as each other.
func (m model) controls(c *canvas, area rect) {
	width := min(area.w, controlsWidth)
	x := area.x + (area.w-width)/2
	m.runKey(c, rect{x, area.y, width, 3})
	m.chips(c, rect{x, area.y + 4, width, 2})
}

// runKey is Start test, Stop test while a run goes, Run again after one; a key that cannot start says why on it.
func (m model) runKey(c *canvas, area rect) {
	p := m.pal
	hovered := m.hover == target{kind: targetKey}
	var labelText, note, cap string
	var plate, faint style
	switch reason, blocked := m.blocked(); {
	case m.running():
		bg := p.raised
		if hovered {
			bg = p.selected
		}
		labelText, cap, plate, faint = "■  Stop test", "esc", p.value.on(bg), p.muted.on(bg)
	case blocked:
		labelText, note, plate, faint = "Start test", reason, p.plateOff, p.plateOff
	default:
		labelText = "Start test"
		if m.run != nil {
			labelText = "Run again"
		}
		plate = p.plate
		if hovered {
			plate.bg = mix(p.canvas, plate.bg, 0.12)
		}
		note, cap, faint = "~"+m.planTime(), "enter", p.plateNote.on(plate.bg)
		plate = plate.withBold()
	}
	c.plate(area, plate.bg)
	text := styled(labelText, plate)
	if note != "" {
		text = text.and("   ", plate).and(note, faint)
	}
	c.center(area.x, area.y+1, area.w, text)
	if cap != "" && area.w >= 30 {
		c.right(area.x, area.y+1, area.w-2, styled(cap, faint))
	}
	c.hit(area, target{kind: targetKey})
}

// blocked is why the key cannot start a test now, if it cannot.
func (m model) blocked() (string, bool) {
	switch {
	case m.running():
		return "", false
	case m.cfg.BaseURL == "":
		return "needs a server", true
	case m.cfg.Validate() != nil:
		return "turn on a stage", true
	}
	return "", false
}

// chipStages are the stages the chips show: the default three, and bidirectional while it is on or ran, in order.
func (m model) chipStages() []goclient.Stage {
	stages := []goclient.Stage{goclient.StageLatency, goclient.StageDownload, goclient.StageUpload}
	if m.stageOn(goclient.StageBidirectional) || slices.ContainsFunc(m.plan(), func(s goclient.StagePlan) bool {
		return s.Name == goclient.StageBidirectional
	}) {
		stages = append(stages, goclient.StageBidirectional)
	}
	return stages
}

// stageOn is whether stage runs: in the run while one goes, else in the settings for the next.
func (m model) stageOn(stage goclient.Stage) bool {
	if m.running() {
		return slices.ContainsFunc(m.plan(), func(s goclient.StagePlan) bool { return s.Name == stage })
	}
	return slices.ContainsFunc(m.cfg.Plan(), func(s goclient.StagePlan) bool { return s.Name == stage })
}

// chips draws a chip per stage: its bar fills as the stage runs, under its mark, name and time or state.
func (m model) chips(c *canvas, area rect) {
	stages := m.chipStages()
	count := len(stages)
	width := max(area.w-(count-1), 0) / max(count, 1)
	for i, stage := range stages {
		x := area.x + i*(width+1)
		w := width
		if i == count-1 {
			w = area.right() - x
		}
		m.chip(c, rect{x, area.y, w, 2}, stage)
	}
}

func (m model) chip(c *canvas, area rect, stage goclient.Stage) {
	p := m.pal
	on := m.stageOn(stage)
	state := stagePending
	if on || m.running() {
		state = m.stageState(stage)
	}
	planned, hasPlan := m.plannedDuration(stage)
	hovered := m.hover == target{kind: targetChip, stage: stage} && !m.running()
	tile := p.surface
	switch {
	case state == stageMeasuring || state == stageWarmup || state == stagePreparing:
		tile = p.wash(stage, 0.16, p.canvas)
	case hovered:
		tile = p.hover
	}
	c.fill(area, tile)
	hue := p.trace(stage)
	track := mix(p.border.fg, tile, 0.75)
	share, bar := 0.0, hue
	switch state {
	case stageMeasuring:
		share = m.elapsed(stage).Seconds() / max(planned.Seconds(), 1e-9)
	case stageDone, stagePartial:
		share = 1
	case stageFailed:
		share, bar = 1, p.err.fg
	case stageStopped:
		share = m.reached(stage)
	}
	inner := max(area.w-2, 0)
	halves := int(math.Round(min(max(share, 0), 1) * float64(inner) * 2))
	rule := line{}
	for cell := range inner {
		switch {
		case (state == stageWarmup || state == stagePreparing) && m.sweeping(cell, inner):
			rule = rule.and("━", fgStyle(hue))
		case state == stagePartial && cell%3 == 2:
			rule = rule.and(" ", style{})
		case 2*cell+2 <= halves:
			rule = rule.and("━", fgStyle(bar))
		case 2*cell+1 == halves:
			rule = rule.and("╸", fgStyle(bar))
		default:
			rule = rule.and("━", fgStyle(track))
		}
	}
	c.text(area.x+1, area.y, inner, rule)
	text, st := m.chipStatus(stage, state, planned, hasPlan && on, inner)
	markStyle, nameStyle := p.stage(stage), p.text
	if !on {
		markStyle, nameStyle = p.muted, p.muted
	}
	name := styled(stageIcons[stage], markStyle).and(" ", style{}).and(stageLabels[stage], nameStyle)
	room := max(inner-styled(text, st).width()-1, 0)
	c.text(area.x+1, area.y+1, room, name.fit(room))
	c.right(area.x+1, area.y+1, inner, styled(text, st))
	if !m.running() {
		c.hit(area, target{kind: targetChip, stage: stage})
	}
}

// chipStatus is the chip's end: the planned time, the time into it, a check, or its state.
func (m model) chipStatus(stage goclient.Stage, state stageState, planned time.Duration, on bool, width int) (string,
	style) {
	p := m.pal
	switch state {
	case stageMeasuring:
		elapsed := strconv.FormatFloat(m.elapsed(stage).Seconds(), 'f', 1, 64)
		if long := elapsed + " / " + fmtSetting(planned); len([]rune(long))+len([]rune(stageLabels[stage]))+5 <= width {
			return long, p.text
		}
		return elapsed + " s", p.text
	case stageWarmup, stagePreparing:
		return "Warmup", p.muted
	case stageDone:
		return "✓", p.ok
	case stagePartial:
		return "Partial", p.warn
	case stageFailed:
		return "Failed", fgStyle(p.err.fg)
	case stageStopped:
		return "Stopped", p.muted
	}
	if !on {
		return "Skipped", p.muted
	}
	return fmtSetting(planned), p.muted
}

// sweeping is whether a warming chip's sweep covers cell of cells now: 45% of the rule crossing it every 1.1 s.
func (m model) sweeping(cell, cells int) bool {
	phase := math.Mod(m.now.Sub(m.since).Seconds()/1.1, 1)
	head := (-0.45 + phase*1.9) * float64(cells)
	at := float64(cell)
	return at >= head && at < head+0.45*float64(cells)
}

// lanesArea draws the latency lanes in area: a rule and head in the latency hue, the column heads, a lane each pitch
// rows, and the axis. A lane is its figures, a box plot (10th to 90th percentile, whiskers to the fastest and slowest,
// the median a tick and the latest reply a mark under it while live) and the latency its load added.
func (m model) lanesArea(c *canvas, area rect, lanes []lane, pitch int) {
	const labelWidth, figureWidth = 15, 10
	p := m.pal
	hue := p.stage(goclient.StageLatency)
	c.text(area.x, area.y, area.w, styled(strings.Repeat("━", area.w), hue))
	c.text(area.x, area.y+1, area.w, styled(stageIcons[goclient.StageLatency], hue).and(" Latency", p.heading))
	if m.multipleRunServers() {
		c.right(area.x, area.y+1, area.w, styled("to "+m.serverName(m.run.latencyServer())+"  l switches", p.muted))
	}
	jitter := area.w >= labelWidth+4*figureWidth+34
	figures := 2
	if jitter {
		figures = 3
	}
	plotX := area.x + labelWidth + figures*figureWidth + 3
	plotWidth := max(area.right()-(plotX+figureWidth+7), 0)
	column := 0
	for _, head := range []string{"Median", "Jitter", "Timeouts"} {
		if head == "Jitter" && !jitter {
			continue
		}
		c.right(area.x+labelWidth+column*figureWidth, area.y+2, figureWidth, styled(head, p.muted))
		column++
	}
	c.right(area.x, area.y+2, area.w, styled("Added", p.muted))
	scale := laneScale(lanes)
	at := func(ms float64) int {
		return plotX + int(math.Round(min(max(ms/scale, 0), 1)*float64(max(plotWidth-1, 0))))
	}
	msOf := func(d time.Duration) float64 { return float64(d) / float64(time.Millisecond) }
	var idle *time.Duration
	for _, l := range lanes {
		if l.stage == goclient.StageLatency && l.stats != nil && l.stats.Count > 0 {
			median := l.stats.P50
			idle = &median
		}
	}
	for i, l := range lanes {
		y := area.y + 3 + i*pitch
		ground := p.canvas
		if l.live {
			ground = p.wash(l.stage, 0.10, p.canvas)
			c.fill(rect{area.x, y, area.w, 1}, ground)
		}
		name := styled(stageIcons[l.stage], p.stage(l.stage)).and(" ", style{}).and(compactPopulation(l.stage), p.text)
		c.text(area.x, y, labelWidth, name)
		stats := l.stats
		if stats != nil && stats.Count+stats.Timeouts == 0 {
			stats = nil
		}
		absent := styled(missing, p.muted)
		cells := []line{absent}
		if stats != nil && stats.Count > 0 {
			cells[0] = styled(fmtMs(stats.P50), p.value)
		}
		if jitter {
			cell := absent
			if stats != nil && stats.JitterPairs > 0 {
				cell = styled(fmtMs(stats.Jitter), p.text)
			}
			cells = append(cells, cell)
		}
		timeouts := absent
		if stats != nil {
			if ratio, ok := stats.TimeoutRatio(); ok {
				st := p.text
				if ratio >= 0.05 {
					st = p.warn
				}
				timeouts = styled(percent(ratio), st)
			}
		}
		cells = append(cells, timeouts)
		for j, cell := range cells {
			c.right(area.x+labelWidth+j*figureWidth, y, figureWidth, cell)
		}
		added := absent
		switch {
		case stats == nil || stats.Count == 0:
		case l.stage == goclient.StageLatency:
			added = styled("Baseline", p.stage(goclient.StageLatency))
		case idle != nil:
			added = styled(fmtAdded(stats.P50-*idle), p.stage(l.stage))
		}
		c.right(area.x, y, area.w, added)
		if stats == nil || stats.Count == 0 || plotWidth < 12 {
			continue
		}
		trace := p.trace(l.stage)
		whisker := fgStyle(mix(trace, ground, 0.7))
		if idle != nil && l.stage != goclient.StageLatency {
			c.text(at(msOf(*idle)), y, 1, styled("┊", fgStyle(mix(p.trace(goclient.StageLatency), ground, 0.55))))
		}
		from, to := at(msOf(stats.Min)), at(min(msOf(stats.Max), scale))
		c.text(from, y, to-from+1, styled(strings.Repeat("─", to-from+1), whisker))
		c.text(from, y, 1, styled("╶", whisker))
		if msOf(stats.Max) > scale {
			end := plotX + plotWidth - 1
			c.text(end, y, 1, styled("▸", fgStyle(trace)))
			c.text(end+2, y, 6, styled(strings.TrimSuffix(fixedMs(stats.Max), " ms"), p.muted))
		} else {
			c.text(to, y, 1, styled("╴", whisker))
		}
		boxFrom, boxTo := at(msOf(stats.P10)), at(min(msOf(stats.P90), scale))
		boxed := mix(trace, ground, 0.38)
		if c.tiles {
			c.fill(rect{boxFrom, y, boxTo - boxFrom + 1, 1}, boxed)
		} else {
			c.text(boxFrom, y, boxTo-boxFrom+1, styled(strings.Repeat("━", boxTo-boxFrom+1), fgStyle(trace)))
		}
		x := at(msOf(stats.P50))
		tick := p.value
		if x >= boxFrom && x <= boxTo {
			tick = tick.on(boxed)
		}
		c.text(x, y, 1, styled("┃", tick))
		if l.live && l.hasLast && pitch == 2 {
			c.text(at(msOf(l.latest)), y+1, 1, styled("▴", fgStyle(trace)))
		}
	}
	if plotWidth >= 12 {
		axis := area.y + 3 + len(lanes)*pitch
		c.text(plotX, axis, 1, styled("0", p.muted))
		c.center(plotX, axis, plotWidth, styled(gaugeTick(scale/2), p.muted))
		c.right(plotX, axis, plotWidth+3, styled(gaugeTick(scale)+" ms", p.muted))
	}
}

// laneScale is the lanes' shared axis: the ladder step over their 90th percentiles with a quarter's room, at least 1
// ms.
func laneScale(lanes []lane) float64 {
	top := 0.0
	for _, l := range lanes {
		if l.stats != nil && l.stats.Count > 0 {
			top = max(top, float64(l.stats.P90)/float64(time.Millisecond))
		}
	}
	return ceilStep(max(top*1.25, 1), 1, 2, 4)
}

// percent is a share of probes as the browser shows it: two decimals under 1%, one from there.
func percent(ratio float64) string {
	if ratio > 0 && ratio < 0.01 {
		return fmt.Sprintf("%.2f%%", ratio*100)
	}
	return fmt.Sprintf("%.1f%%", ratio*100)
}

// cards draws a card per stage, as many to a row as keep each at least 30 columns wide; a short area keeps them in one
// row.
func (m model) cards(c *canvas, area rect) {
	plan := m.plan()
	if len(plan) == 0 || area.h < 2 {
		return
	}
	narrowest := 30
	if area.h < 16 {
		narrowest = 18
	}
	per := len(plan)
	for per > 1 && max(area.w-2*(per-1), 0)/per < narrowest {
		per = (per + 1) / 2
	}
	rows := (len(plan) + per - 1) / per
	height := max(area.h-(rows-1), 0) / rows
	width := max(area.w-2*(per-1), 0) / per
	sc := m.scales()
	for i, stage := range plan {
		column, row := i%per, i/per
		x := area.x + column*(width+2)
		w := width
		if column == per-1 {
			w = area.right() - x
		}
		m.card(c, rect{x, area.y + row*(height+1), w, height}, stage, sc)
	}
}

// scales are the cards' tops: the rate charts' shared one, and the reply tracks', the idle one's its own and the
// loaded ones' shared, each the ladder step over 2.5× the 75th percentile of its replies' slowest, so their bodies
// keep their shape.
func (m model) scales() scales {
	r := m.run
	if r == nil {
		return scales{rate: niceCeil(8*1.03) / 8, idle: 1, loaded: 1}
	}
	series := r.rtt[r.latencyServer()]
	top := func(idle bool) float64 {
		var highs []float64
		for _, stage := range r.plan {
			if (stage.Name == goclient.StageLatency) != idle {
				continue
			}
			if t0, t1, ok := r.window(stage.Name); ok {
				for _, pt := range inWindow(series.points, t0, t1) {
					if !math.IsNaN(pt.v) {
						highs = append(highs, pt.peak)
					}
				}
			}
		}
		slices.Sort(highs)
		p75 := 0.0
		if len(highs) > 0 {
			p75 = highs[len(highs)*3/4]
		}
		return ceilStep(max(p75*2.5, 1), 1, 2, 4)
	}
	return scales{rate: r.stripTop(), idle: top(true), loaded: top(false)}
}

// card draws one stage's card: a head washed in its hue while it runs and once done, its figure, a quiet line, its
// traces on a field of its hue, and its facts. Its layout never changes, so nothing moves between Start and the
// result.
func (m model) card(c *canvas, area rect, stage goclient.StagePlan, sc scales) {
	p := m.pal
	state := m.stageState(stage.Name)
	tile := p.surface
	c.fill(area, tile)
	head := p.raised
	switch state {
	case stageMeasuring, stageWarmup, stagePreparing:
		head = p.wash(stage.Name, 0.24, tile)
	case stageDone, stagePartial, stageFailed, stageStopped:
		head = p.wash(stage.Name, 0.12, tile)
	}
	c.fill(rect{area.x, area.y, area.w, 1}, head)
	inner := max(area.w-2, 0)
	c.text(area.x+1, area.y, inner, styled(stageIcons[stage.Name], p.stage(stage.Name)).and(" ", style{}).
		and(stageLabels[stage.Name], p.heading))
	status, st := m.cardStatus(state)
	c.right(area.x+1, area.y, inner, styled(status, st))
	u := unitOf(m.peak())
	// Head, figure, quiet line, the traces' field and the facts; a roomy card adds a row of air around them, and a short
	// one gives up its traces, then the facts that do not fit.
	roomy := 0
	if area.h >= 16 {
		roomy = 1
	}
	y := area.y + 1 + roomy
	for _, l := range m.cardLines(stage.Name, state, u, inner) {
		if y < area.bottom() {
			c.text(area.x+1, y, inner, l)
		}
		y++
	}
	facts := m.facts(stage.Name, u)
	facts = facts[:min(len(facts), max(area.bottom()-(y+roomy), 0))]
	first := area.bottom() - len(facts) - roomy
	top, bottom := y+roomy, first
	if len(facts) > 0 {
		bottom--
	}
	if bottom >= top+2 {
		field := rect{area.x + 1, top, inner, bottom - top}
		c.fill(field, p.wash(stage.Name, 0.05, tile))
		m.traces(c, field, stage, sc)
	}
	for i, f := range facts {
		st := p.text
		if f[1] == missing {
			st = p.muted
		}
		value := styled(f[1], st)
		room := max(inner-value.width()-1, 0)
		c.text(area.x+1, first+i, room, styled(f[0], p.muted).fit(room))
		c.right(area.x+1, first+i, inner, value)
	}
}

// cardStatus is the card's head end: what the stage is doing, or how it ended.
func (m model) cardStatus(state stageState) (string, style) {
	p := m.pal
	switch state {
	case stageMeasuring:
		return "Running", p.text
	case stageWarmup, stagePreparing:
		return "Warming up", p.muted
	case stageDone:
		return "✓", p.ok
	case stagePartial:
		return "Partial", p.warn
	case stageFailed:
		return "Failed", fgStyle(p.err.fg)
	case stageStopped:
		return "Stopped", p.muted
	}
	return "", p.muted
}

// cardLines are the card's figure, and its quiet line: the wire rate, the jitter, a failure's reason or the warmup.
func (m model) cardLines(stage goclient.Stage, state stageState, u unit, width int) [2]line {
	p := m.pal
	absent := styled(missing, p.muted)
	figure := func(value, name string) line { return styled(value, p.value).and(" "+name, p.muted) }
	var quiet line
	value := absent
	if stage == goclient.StageLatency {
		stats := m.population(stage)
		if stats != nil && stats.JitterPairs > 0 {
			quiet = styled("jitter ", p.muted).and(fmtMs(stats.Jitter), p.text)
		}
		switch latest, live := m.latest(); {
		case stats != nil && stats.Count > 0:
			value = figure(strings.TrimSuffix(fmtMs(stats.P50), " ms"), "ms")
		case live && state == stageMeasuring:
			value = figure(strings.TrimSuffix(fmtMs(latest), " ms"), "ms")
		}
	} else {
		rate := func(dir goclient.Direction) (float64, bool) {
			for _, result := range m.runResults() {
				if result.Stage == stage && result.Direction == dir && !result.Unavailable {
					return result.MeanBps, true
				}
			}
			if state == stageMeasuring && m.run != nil {
				if g := m.run.shown[dir]; g.to > 0 {
					return g.value(m.now), true
				}
			}
			return 0, false
		}
		hue := p.stage(stage)
		if stage == goclient.StageBidirectional {
			down, okDown := rate(goclient.Down)
			up, okUp := rate(goclient.Up)
			if okDown || okUp {
				side := func(arrow string, v float64, ok bool) line {
					shown := absent
					if ok {
						shown = styled(u.figure(v), p.value)
					}
					return styled(arrow, hue).with(shown)
				}
				value = side("↓ ", down, okDown).and("   ", style{}).with(side("↑ ", up, okUp)).and(" "+u.name, p.muted)
			}
		} else {
			dir := goclient.Down
			if stage == goclient.StageUpload {
				dir = goclient.Up
			}
			if v, ok := rate(dir); ok {
				value = figure(u.figure(v), u.name)
			}
		}
	}
	if state == stageDone && m.run != nil {
		mean := m.run.mean(stage)
		if wire, ok := wireRate(m.run.details, stage); ok && wire >= mean*1.005 && mean > 0 {
			quiet = styled("wire ", p.muted).and(u.figure(wire)+" "+u.name, p.text)
			if overhead := fmt.Sprintf("  +%.1f%%", (wire/mean-1)*100); quiet.width()+len(overhead) <= width {
				quiet = quiet.and(overhead, p.muted)
			}
		}
	}
	switch state {
	case stageFailed:
		quiet = styled(m.stageFailure(stage, "Failed"), fgStyle(p.err.fg))
	case stagePartial:
		quiet = styled(m.stageFailure(stage, "Partial"), p.warn)
	case stageWarmup, stagePreparing:
		quiet = styled(m.spinner(), p.accent).and(" not counted yet", p.muted)
	}
	return [2]line{value, quiet}
}

// stageFailure is why the stage failed: the latency server's first failure, else any server's, else fallback.
func (m model) stageFailure(stage goclient.Stage, fallback string) string {
	if m.run == nil || m.run.details == nil {
		return fallback
	}
	var any string
	for _, f := range m.run.details.Failures {
		if f.Stage != stage {
			continue
		}
		label := failureLabels[f.Reason]
		if f.ServerID == m.run.latencyServer() {
			return label
		}
		if any == "" {
			any = label
		}
	}
	if any != "" {
		return any
	}
	return fallback
}

// facts are the card's facts as label and value, "—" until known.
func (m model) facts(stage goclient.Stage, u unit) [][2]string {
	if stage == goclient.StageLatency {
		facts := [][2]string{{"Range", missing}, {"95th percentile", missing}, {"Replies", missing}, {"Timeouts", missing}}
		if stats := m.population(stage); stats != nil {
			if stats.Count > 0 {
				facts[0][1] = strings.TrimSuffix(fmtMs(stats.Min), " ms") + "–" + fixedMs(stats.Max)
				facts[1][1] = fmtMs(stats.P95)
				facts[2][1] = fmtCount(stats.Count)
			}
			if ratio, ok := stats.TimeoutRatio(); ok {
				facts[3][1] = percent(ratio)
			}
		}
		return facts
	}
	result := func(dir goclient.Direction) *goclient.Result {
		for _, r := range m.runResults() {
			if r.Stage == stage && r.Direction == dir && r.Elapsed > 0 {
				return &r
			}
		}
		return nil
	}
	bytes := func(dir goclient.Direction) string {
		if r := result(dir); r != nil {
			return fmtBytes(r.TotalBytes)
		}
		return missing
	}
	duration := missing
	if stage == goclient.StageBidirectional {
		if r := result(goclient.Down); r != nil {
			duration = fmtClock(r.Elapsed)
		}
		return [][2]string{{"↓ Transferred", bytes(goclient.Down)}, {"↑ Transferred", bytes(goclient.Up)},
			{"Duration", duration}}
	}
	dir := goclient.Down
	if stage == goclient.StageUpload {
		dir = goclient.Up
	}
	peak := missing
	if r := result(dir); r != nil {
		duration = fmtClock(r.Elapsed)
		if !r.Unavailable && r.PeakBps > 0 {
			peak = u.figure(r.PeakBps) + " " + u.name
		}
	}
	return [][2]string{{"Peak", peak}, {"Transferred", bytes(dir)}, {"Duration", duration}}
}

// traces draws the stage's traces on its field: a transfer's rate line over its loaded replies, or the idle replies
// alone.
func (m model) traces(c *canvas, field rect, stage goclient.StagePlan, sc scales) {
	r := m.run
	if r == nil {
		return
	}
	t0, _, ok := r.window(stage.Name)
	if !ok {
		return
	}
	t1 := t0 + stage.Duration.Seconds()
	p := m.pal
	hue := p.trace(stage.Name)
	series, probed := r.rtt[r.latencyServer()]
	if stage.Name == goclient.StageLatency {
		if probed {
			bars(c, field, inWindow(series.points, t0, t1), sc.idle, t0, t1, hue)
		}
		return
	}
	loaded := m.cfg.LoadedLatency && probed && field.h >= 5
	rates := field
	if loaded {
		rates.h -= 2
	}
	live := m.stageState(stage.Name) == stageMeasuring
	for i, dir := range stage.Directions {
		var head *liveHead
		if g := r.shown[dir]; live && g.to > 0 {
			head = &liveHead{m.now.Sub(r.started).Seconds(), g.value(m.now)}
		}
		col := hue
		var fill = p.wash(stage.Name, 0.05, p.surface)
		if i > 0 {
			col, fill = mix(hue, p.surface, 0.55), nil
		}
		traceLine(c, rates, inWindow(r.history[dir].points, t0, t1), head, sc.rate, t0, t1, col, p.text.fg, fill)
	}
	if loaded {
		track := rect{field.x, field.bottom() - 2, field.w, 2}
		bars(c, track, inWindow(series.points, t0, t1), sc.loaded, t0, t1, mix(hue, p.surface, 0.8))
	}
}

// dialSpec is what the dial shows: the live stage while one measures, the result once the run ends, at rest
// otherwise.
func (m model) dialSpec() dial {
	p := m.pal
	peak := m.peak()
	u := unitOf(peak)
	gauge := gaugeCeiling(12.5e6)
	if peak > 0 {
		gauge = gaugeCeiling(peak)
	}
	var rateTicks, msTicks [5]string
	msScale := 1.0
	if m.run != nil {
		msScale = max(float64(m.run.rttScale)/float64(time.Millisecond), 1)
	}
	for i := range rateTicks {
		rateTicks[i] = gaugeTick(gaugeValue(float64(i)/4, gauge) * 8 / u.divisor)
		msTicks[i] = gaugeTick(float64(i) / 4 * msScale)
	}
	label := func(stage goclient.Stage) line {
		return styled(stageIcons[stage]+" "+stageLabels[stage], p.stage(stage))
	}
	atRest := func(ticks [5]string, note string, st style) dial {
		return dial{track: p.border.fg, ticks: ticks, value: missing, note: styled(note, st)}
	}
	r := m.run
	if m.next != nil {
		return atRest(rateTicks, "Verifying the selected paths", p.muted)
	}
	if r == nil {
		if m.cfg.BaseURL == "" {
			return atRest(rateTicks, "Add a server to measure your connection", p.muted)
		}
		return atRest(rateTicks, "Ready to measure your connection", p.muted)
	}
	if r.running() {
		stage := r.stage
		if stage == "" || r.phase == goclient.PhasePreparing && !slices.ContainsFunc(r.stages, func(s stageProgress) bool {
			return s.name == stage
		}) {
			return atRest(rateTicks, "Verifying the selected paths", p.muted)
		}
		track := p.wash(stage, 0.22, p.border.fg)
		ticks := rateTicks
		if stage == goclient.StageLatency {
			ticks = msTicks
		}
		needle := func(to float64) []arc { return []arc{{to: r.needleAt(to, m.now), color: p.trace(stage)}} }
		if r.phase != goclient.PhaseMeasuring {
			d := atRest(ticks, "Warming up; not counted", p.muted)
			d.track, d.label, d.arcs = track, label(stage), needle(0)
			return d
		}
		if stage == goclient.StageLatency {
			latest, ok := m.latest()
			d := dial{track: track, ticks: ticks, label: label(stage), value: missing, unit: "ms", note: m.timeoutStreak()}
			if ok {
				d.arcs = needle(float64(latest) / float64(time.Millisecond) / msScale)
				d.value = strings.TrimSuffix(fmtMs(latest), " ms")
			} else {
				d.arcs = needle(0)
			}
			return d
		}
		value := 0.0
		for _, dir := range []goclient.Direction{goclient.Down, goclient.Up} {
			if slices.Contains(r.directions(stage), dir) {
				value += r.shown[dir].value(m.now)
			}
		}
		d := dial{arcs: needle(gaugeFraction(value, gauge)), track: track, ticks: ticks, label: label(stage),
			value: missing, unit: u.name}
		if value > 0 {
			d.value = u.figure(value)
		}
		return d
	}
	// Finished: every transfer stage's arc, swept in from where the needle stood, and the first one's figure.
	var arcs []arc
	var first *goclient.Stage
	var firstMean float64
	for _, stage := range r.plan {
		mean := r.mean(stage.Name)
		if stage.Name == goclient.StageLatency || mean <= 0 {
			continue
		}
		arcs = append(arcs, arc{to: r.swept(gaugeFraction(mean, gauge), m.now), color: p.trace(stage.Name)})
		if first == nil {
			name := stage.Name
			first, firstMean = &name, mean
		}
	}
	if first != nil {
		return dial{arcs: arcs, track: p.border.fg, ticks: rateTicks, label: label(*first), value: u.figure(firstMean),
			unit: u.name, note: m.outcomeNote()}
	}
	if stats := m.population(goclient.StageLatency); stats != nil && stats.Count > 0 {
		ms := float64(stats.P50) / float64(time.Millisecond)
		return dial{arcs: []arc{{to: ms / msScale, color: p.trace(goclient.StageLatency)}}, track: p.border.fg,
			ticks: msTicks, label: label(goclient.StageLatency), value: strings.TrimSuffix(fmtMs(stats.P50), " ms"),
			unit: "ms", note: m.outcomeNote()}
	}
	d := atRest(rateTicks, "", p.muted)
	d.note = m.outcomeNote()
	return d
}

// outcomeNote is under a finished run's dial: why it stopped or failed, if it did.
func (m model) outcomeNote() line {
	if m.run == nil {
		return nil
	}
	switch m.run.outcome {
	case goclient.OutcomeStopped:
		return styled("Stopped early", m.pal.muted)
	case goclient.OutcomeFailed:
		text := "Nothing was measured"
		if m.run.err != nil {
			text = errorText(m.run.err)
		}
		return styled(text, m.pal.warn)
	}
	return nil
}

// lanes are a lane for idle latency and, with loaded latency on, each transfer stage, in plan order.
func (m model) lanes() []lane {
	var lanes []lane
	for _, stage := range m.plan() {
		if stage.Name != goclient.StageLatency && !m.cfg.LoadedLatency {
			continue
		}
		live := m.stageState(stage.Name) == stageMeasuring
		l := lane{stage: stage.Name, stats: m.population(stage.Name), live: live}
		if latest, ok := m.latest(); ok && live {
			l.latest, l.hasLast = latest, true
		}
		lanes = append(lanes, l)
	}
	return lanes
}

// inWindow is the points over [t0, t1] seconds.
func inWindow(points []point, t0, t1 float64) []point {
	from, _ := slices.BinarySearchFunc(points, t0, func(p point, t float64) int {
		if p.t < t {
			return -1
		}
		return 1
	})
	to, _ := slices.BinarySearchFunc(points, t1+1e-9, func(p point, t float64) int {
		if p.t < t {
			return -1
		}
		return 1
	})
	return points[from:to]
}
