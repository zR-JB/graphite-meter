package main

import (
	"cmp"
	"math"
	"slices"
	"strconv"
	"strings"
	"time"

	"charm.land/lipgloss/v2"
	"github.com/charmbracelet/x/ansi"
	"github.com/zR-JB/graphite-meter/go/internal/goclient"
)

// The terminal console draws what the browser's does (docs/DESIGN.md): a dial, latency lanes, a key, the stage
// track and a card per stage, flat, with colour only where it names a stage or a state.

// The browser's dial transfer curve (client/src/lib/components/gaugeScale.ts): equal sweeps for each knot.
var gaugeKnots = [...]float64{0, 0.005, 0.01, 0.05, 0.1, 0.25, 0.5, 0.75, 1}

func gaugeFraction(value, scale float64) float64 {
	v := min(max(value/scale, 0), 1)
	if math.IsNaN(v) {
		return 0
	}
	for i := 1; i < len(gaugeKnots); i++ {
		if v <= gaugeKnots[i] {
			return (float64(i-1) + (v-gaugeKnots[i-1])/(gaugeKnots[i]-gaugeKnots[i-1])) / float64(len(gaugeKnots)-1)
		}
	}
	return 1
}

func gaugeValue(fraction, scale float64) float64 {
	at := min(max(fraction, 0), 1) * float64(len(gaugeKnots)-1)
	i := min(int(at), len(gaugeKnots)-2)
	return scale * (gaugeKnots[i] + (at-float64(i))*(gaugeKnots[i+1]-gaugeKnots[i]))
}

// ceilStep is the browser's axis ceiling (client/src/lib/presentation/scales.ts): the first step at or above v.
func ceilStep(v float64, steps ...float64) float64 {
	if !(v > 0) || math.IsInf(v, 0) {
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
	places := min(6, max(0, 2-int(math.Floor(math.Log10(math.Abs(v))))))
	text := strconv.FormatFloat(v, 'f', places, 64)
	if strings.Contains(text, ".") {
		text = strings.TrimRight(strings.TrimRight(text, "0"), ".")
	}
	return text
}

type arc struct {
	to  float64 // 0–1 of the sweep
	hue lipgloss.Style
}

type dial struct {
	arcs         []arc
	ticks        [5]string
	label, value string
	unit, note   string // The note is styled; it sits under the unit.
	hue          lipgloss.Style
}

// The dial opens downward over 270°, as the browser's does.
const dialStart, dialTurn = 225.0, 270.0

// dial draws the ring in braille with its ticks outside it and the readout in large figures inside it. Where arcs
// overlap the shorter one shows, so every arc's head stays visible.
func (s styles) dial(d dial, w, h int) []string {
	const margin = 5
	cols, rows := max(w-2*margin, 12), max(h-1, 6)
	dotW, dotH := cols*2, rows*4
	radius := min(float64(dotW)/2-1, (float64(dotH)-2)/(1+math.Sin(math.Pi/4)))
	cx, cy := float64(dotW)/2, radius+1
	thick := max(radius/9, 2)
	dots := make([]rune, cols*rows)
	owner := make([]int, cols*rows)
	for i := range owner {
		owner[i] = -1
	}
	for y := range dotH {
		for x := range dotW {
			dx, dy := float64(x)+0.5-cx, cy-(float64(y)+0.5)
			if math.Abs(math.Hypot(dx, dy)-radius) > thick/2 {
				continue
			}
			f := math.Mod(dialStart-math.Atan2(dy, dx)*180/math.Pi+360, 360) / dialTurn
			if f > 1 {
				continue
			}
			cell := y/4*cols + x/2
			dots[cell] |= brailleDots[y%4][x%2]
			for i, a := range d.arcs {
				if f <= a.to && (owner[cell] < 0 || a.to < d.arcs[owner[cell]].to) {
					owner[cell] = i
				}
			}
		}
	}
	readout := []string{d.hue.Render(d.label), ""}
	for _, row := range bigFigures(d.value) {
		readout = append(readout, s.value.Render(row))
	}
	readout = append(readout, s.muted.Render(d.unit), d.note)
	first := int(cy/4) - len(readout)/2 + 1
	lines := make([]string, rows)
	for r := range rows {
		var b strings.Builder
		text, textAt, textW := "", -1, 0
		if i := r - first; i >= 0 && i < len(readout) {
			text, textW = readout[i], lipgloss.Width(readout[i])
			textAt = (cols - textW) / 2
		}
		for c := 0; c < cols; c++ {
			if c == textAt && textW > 0 {
				b.WriteString(text)
				c += textW - 1
				continue
			}
			cell := r*cols + c
			switch {
			case dots[cell] == 0:
				b.WriteByte(' ')
			case owner[cell] >= 0:
				b.WriteString(d.arcs[owner[cell]].hue.Render(string(0x2800 + dots[cell])))
			default:
				b.WriteString(s.border.Render(string(0x2800 + dots[cell])))
			}
		}
		lines[r] = b.String()
	}
	// Ticks sit outside the ring at 0, ¼, ½, ¾ and the full sweep.
	left, right := make([]string, rows), make([]string, rows)
	out := []string{""}
	for i, f := range [5]float64{0, 0.25, 0.5, 0.75, 1} {
		label := s.muted.Render(ansi.Truncate(d.ticks[i], margin-1, ""))
		a := (dialStart - f*dialTurn) * math.Pi / 180
		x, y := (cx+(radius+thick+3)*math.Cos(a))/2, (cy-(radius+thick+3)*math.Sin(a))/4
		row := min(max(int(y), 0), rows-1)
		switch {
		case i == 2:
			out[0] = lipgloss.PlaceHorizontal(cols+2*margin, lipgloss.Center, label)
		case x < float64(cols)/2:
			left[row] = label
		default:
			right[row] = label
		}
	}
	for r := range rows {
		out = append(out, lipgloss.PlaceHorizontal(margin, lipgloss.Right, left[r]+" ")+lines[r]+" "+right[r])
	}
	return out
}

// readout is the dial without its ring, for a console too small for one.
func (s styles) readout(d dial) []string {
	out := []string{d.hue.Render(d.label)}
	for i, row := range bigFigures(d.value) {
		line := s.value.Render(row)
		if i == 2 {
			line += "  " + s.muted.Render(d.unit) + "  " + d.note
		}
		out = append(out, line)
	}
	return out
}

// Figures three rows tall, in rounded strokes.
var figureGlyphs = map[rune][3]string{
	'0': {"╭─╮", "│ │", "╰─╯"}, '1': {"╶┐ ", " │ ", "╶┴╴"}, '2': {"╶─╮", "╭─╯", "╰─╴"},
	'3': {"╶─╮", " ─┤", "╶─╯"}, '4': {"╷ ╷", "╰─┤", "  ╵"}, '5': {"╭─╴", "╰─╮", "╶─╯"},
	'6': {"╭─╴", "├─╮", "╰─╯"}, '7': {"╶─┐", "  │", "  ╵"}, '8': {"╭─╮", "├─┤", "╰─╯"},
	'9': {"╭─╮", "╰─┤", "╶─╯"}, '.': {" ", " ", "•"}, '<': {"  ", "╱ ", "╲ "}, '—': {"   ", "───", "   "},
	' ': {" ", " ", " "},
}

func bigFigures(text string) [3]string {
	var rows [3]strings.Builder
	for i, r := range text {
		glyph, ok := figureGlyphs[r]
		if !ok {
			continue
		}
		for row := range 3 {
			if i > 0 {
				rows[row].WriteByte(' ')
			}
			rows[row].WriteString(glyph[row])
		}
	}
	return [3]string{rows[0].String(), rows[1].String(), rows[2].String()}
}

var rises = [9]rune{' ', '▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'}

// strip fills a stage's rate over its window to an eighth of a cell, fading by row from the stage's hue into the
// canvas as the browser's strip does. A second band (bidirectional upload) stacks on the first in a flat tint;
// where they meet the lower band rises over the upper as background. An empty column keeps a baseline.
func (s styles) strip(stage goclient.Stage, bands [][]point, top, t0, t1 float64, cols, rows int) []string {
	heights := make([][2]float64, cols)
	for b, band := range bands[:min(len(bands), 2)] {
		sum, n := make([]float64, cols), make([]int, cols)
		for _, p := range band {
			if x := int((p.t - t0) / (t1 - t0) * float64(cols)); x >= 0 && x < cols && !math.IsNaN(p.v) {
				sum[x], n[x] = sum[x]+p.v*float64(p.n), n[x]+p.n
			}
		}
		for x := range cols {
			if n[x] > 0 {
				heights[x][b] = min(sum[x]/float64(n[x])/top, 1) * float64(rows*8)
			}
		}
	}
	shades := s.shades[stage]
	upper := shades[1]
	lines := make([]string, rows)
	for r := range rows {
		floor := float64((rows - 1 - r) * 8)
		shade := shades[min(r*len(shades)/rows, len(shades)-1)]
		var b, run strings.Builder
		paint := lipgloss.Style{}
		for x := range cols {
			lower, total := heights[x][0], heights[x][0]+heights[x][1]
			stacked := heights[x][1] > 0
			glyph, style := ' ', shade
			switch {
			case total <= floor && r == rows-1:
				glyph, style = '▁', s.border
			case total <= floor:
			case total <= floor+8:
				glyph = rises[max(int(math.Round(total-floor)), 1)]
				if stacked && lower <= floor+4 {
					style = upper
				}
			case stacked && lower > floor && lower < floor+8:
				glyph, style = rises[max(int(math.Round(lower-floor)), 1)], shade.Background(upper.GetForeground())
			case stacked && lower <= floor:
				glyph, style = '█', upper
			default:
				glyph = '█'
			}
			if style.GetForeground() != paint.GetForeground() || style.GetBackground() != paint.GetBackground() {
				if run.Len() > 0 {
					b.WriteString(paint.Render(run.String()))
					run.Reset()
				}
				paint = style
			}
			run.WriteRune(glyph)
		}
		b.WriteString(paint.Render(run.String()))
		lines[r] = b.String()
	}
	return lines
}

var brailleDots = [4][2]rune{{0x01, 0x08}, {0x02, 0x10}, {0x04, 0x20}, {0x40, 0x80}}

// line draws points over a window as a braille line, broken where a value is missing.
func (s styles) line(points []point, top, t0, t1 float64, cols, rows int, style lipgloss.Style) []string {
	dotW, dotH := cols*2, rows*4
	dots := make([]rune, cols*rows)
	set := func(x, y int) { dots[y/4*cols+x/2] |= brailleDots[y%4][x%2] }
	px, py, drawn := 0, 0, false
	for _, p := range points {
		x := int((p.t - t0) / (t1 - t0) * float64(dotW))
		if math.IsNaN(p.v) || x < 0 || x >= dotW {
			drawn = false
			continue
		}
		y := dotH - 1 - int(math.Round(min(max(p.v/top, 0), 1)*float64(dotH-1)))
		if !drawn {
			px, py = x, y
		}
		// Bresenham from the previous point.
		dx, dy, sx, sy := abs(x-px), -abs(y-py), cmp.Compare(x, px), cmp.Compare(y, py)
		for e := dx + dy; ; {
			set(px, py)
			if px == x && py == y {
				break
			}
			e2 := 2 * e
			if e2 >= dy {
				e, px = e+dy, px+sx
			}
			if e2 <= dx {
				e, py = e+dx, py+sy
			}
		}
		drawn = true
	}
	lines := make([]string, rows)
	for r := range rows {
		var b strings.Builder
		for c := range cols {
			switch d := dots[r*cols+c]; {
			case d != 0:
				b.WriteString(style.Render(string(0x2800 + d)))
			case r == rows-1:
				b.WriteString(s.border.Render("▁"))
			default:
				b.WriteByte(' ')
			}
		}
		lines[r] = b.String()
	}
	return lines
}

func abs(n int) int { return max(n, -n) }

var stageIcons = map[goclient.Stage]string{
	goclient.StageLatency: "≈", goclient.StageDownload: "↓", goclient.StageUpload: "↑", goclient.StageBidirectional: "↕",
}

// card is a stage's panel: a rule in its hue over its title, as the browser's cards are.
func (s styles) card(stage goclient.Stage, body []string, w int) []string {
	hue := s.stage[stage]
	out := []string{hue.Render(strings.Repeat("━", w)), hue.Bold(true).Render(stageIcons[stage] + " " + stageLabels[stage])}
	for _, line := range body {
		out = append(out, fit(line, w))
	}
	return out
}

// fact is a label and its figure on one row, the figure flush right.
func (s styles) fact(label, value string, w int) string {
	return s.muted.Render(label) + strings.Repeat(" ", max(w-lipgloss.Width(label)-lipgloss.Width(value), 1)) +
		s.text.Render(value)
}

// key is the transport key: a plate in ink three rows tall, its key cap at the right.
func (s styles) key(label, note, cap string, w int, enabled bool) []string {
	plate, faint := s.plate, s.plateNote
	if !enabled {
		plate, faint = s.plateOff, s.plateOff
	}
	text := plate.Bold(true).Render(label)
	if note != "" {
		text += plate.Render("  ") + faint.Render(note)
	}
	capW := lipgloss.Width(cap) + 2
	gap := max(w-lipgloss.Width(text)-2*capW, 0)
	middle := plate.Render(strings.Repeat(" ", capW+gap/2)) + text + plate.Render(strings.Repeat(" ", gap-gap/2)) +
		faint.Render(cap) + plate.Render("  ")
	blank := plate.Render(strings.Repeat(" ", max(w, lipgloss.Width(middle))))
	return []string{blank, middle, blank}
}

type lane struct {
	stage goclient.Stage
	stats *goclient.LatencyStats // Nil until measured; the styled note says why.
	note  string
}

// lanes is the latency table: figures beside a lane from the median to the 95th percentile, the idle median marked
// down every lane so that added latency reads as a distance. Under ten columns the lane gives way to the figures.
func (s styles) lanes(rows []lane, w int) []string {
	const labelW, figureW = 14, 10
	laneW := w - labelW - 4*figureW - 2
	if laneW < 10 {
		laneW = 0
	}
	scale, idle, measured := 0.0, time.Duration(-1), false
	for _, row := range rows {
		if row.stats != nil {
			measured = true
			scale = max(scale, float64(max(row.stats.P50, row.stats.P95))/1e6)
			if row.stage == goclient.StageLatency {
				idle = row.stats.P50
			}
		}
	}
	scale = ceilStep(max(scale*1.1, 1), 1, 2, 4)
	at := func(d time.Duration) int { return min(max(int(float64(d)/1e6/scale*float64(laneW-1)+0.5), 0), laneW-1) }
	gutter := "  "
	if laneW == 0 {
		gutter = ""
	}
	right := func(text string, style lipgloss.Style) string {
		return style.Render(lipgloss.PlaceHorizontal(figureW, lipgloss.Right, text))
	}
	// Until a row is measured the rows say why, without headings over empty columns.
	var out []string
	if measured {
		out = append(out, strings.Repeat(" ", labelW)+right("Median", s.muted)+right("Jitter", s.muted)+
			right("Timeouts", s.muted)+gutter+strings.Repeat(" ", laneW)+right("Added", s.muted))
	}
	for _, row := range rows {
		label := s.stage[row.stage].Render(stageIcons[row.stage]+" ") + s.text.Render(pad(compactPopulation(row.stage), labelW-2))
		if row.stats == nil {
			out = append(out, label+row.note)
			continue
		}
		st, hue := *row.stats, s.trace[row.stage]
		timeouts, jitter := missing, missing
		if ratio, ok := st.TimeoutRatio(); ok {
			timeouts = strconv.FormatFloat(ratio*100, 'f', 1, 64) + "%"
		}
		if st.JitterPairs > 0 {
			jitter = fmtMs(st.Jitter)
		}
		added := ""
		if idle >= 0 && row.stage != goclient.StageLatency {
			added = fmtAdded(st.P50 - idle)
		}
		track := slices.Repeat([]string{" "}, laneW)
		if laneW > 0 {
			from, mid, to := at(idle), at(st.P50), at(st.P95)
			if added != "" {
				for i := min(from, mid); i < max(from, mid); i++ {
					track[i] = hue.Render("─")
				}
				track[from] = s.stage[goclient.StageLatency].Render("┊")
			}
			for i := mid + 1; i <= to; i++ {
				track[i] = hue.Render("━")
			}
			track[mid] = hue.Render("●")
		}
		out = append(out, label+right(fmtMs(st.P50), s.value)+right(jitter, s.text)+right(timeouts, s.text)+gutter+
			strings.Join(track, "")+right(added, s.stage[row.stage]))
	}
	if laneW == 0 || !measured {
		return out
	}
	end := gaugeTick(scale) + " ms"
	axis := "0" + strings.Repeat(" ", max(laneW-1-lipgloss.Width(end), 1)) + end
	return append(out, strings.Repeat(" ", labelW+3*figureW+2)+s.muted.Render(axis))
}

type chip struct {
	stage    goclient.Stage
	progress float64 // 0–1 of the stage's run
	status   string
}

// chips is the stage track: each stage's rule fills in its hue as the stage runs, over its name and status.
func (s styles) chips(chips []chip, w int) []string {
	if len(chips) == 0 {
		return nil
	}
	const gap = 2
	cw := (w - gap*(len(chips)-1)) / len(chips)
	// A cramped track names every stage in its hue instead of beside its icon.
	icons := true
	for _, c := range chips {
		icons = icons && lipgloss.Width(stageLabels[c.stage])+lipgloss.Width(c.status)+3 <= cw
	}
	var rule, name strings.Builder
	for i, c := range chips {
		if i > 0 {
			rule.WriteString(strings.Repeat(" ", gap))
			name.WriteString(strings.Repeat(" ", gap))
		}
		lit := int(math.Round(min(max(c.progress, 0), 1) * float64(cw)))
		rule.WriteString(s.stage[c.stage].Render(strings.Repeat("━", lit)) + s.border.Render(strings.Repeat("━", cw-lit)))
		room := cw - lipgloss.Width(c.status) - 1
		title := s.stage[c.stage].Render(stageIcons[c.stage]) + " " + s.text.Render(stageLabels[c.stage])
		if !icons {
			title = s.stage[c.stage].Render(compactStage(c.stage))
		}
		title = ansi.Truncate(title, max(room, 1), "…")
		name.WriteString(title + strings.Repeat(" ", max(cw-lipgloss.Width(title)-lipgloss.Width(c.status), 1)) + c.status)
	}
	return []string{rule.String(), name.String()}
}

// section is a neutral panel: a hairline rule over its title, as a card is without a stage.
func (s styles) section(title string, w int) []string {
	return []string{s.border.Render(strings.Repeat("━", w)), s.heading.Render(ansi.Truncate(title, w, "…"))}
}
