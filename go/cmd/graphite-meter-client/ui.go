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

func fit(s string, w int) string {
	lines := strings.Split(s, "\n")
	for i, line := range lines {
		lines[i] = ansi.Truncate(line, max(w, 1), "…")
	}
	return strings.Join(lines, "\n")
}

func (s styles) panel(title, body string, w, h int) string {
	inner := max(w-4, 1)
	lines := strings.Split(body, "\n")
	rows := len(lines)
	if h > 0 {
		rows = max(h-2, 1)
	}
	title = ansi.Truncate(title, max(w-6, 1), "…")
	fill := max(w-5-lipgloss.Width(title), 0)
	top := s.border.Render("╭─ ") + s.heading.Render(title) + s.border.Render(" "+strings.Repeat("─", fill)+"╮")
	edge := s.border.Render("│")
	var b strings.Builder
	b.Grow((rows+2)*(w+32) + len(body))
	b.WriteString(top)
	for i := range rows {
		line := ""
		if i < len(lines) {
			line = lines[i]
		}
		width := ansi.StringWidth(line)
		if width > inner {
			line = ansi.Truncate(line, inner, "…")
			width = ansi.StringWidth(line)
		}
		b.WriteString("\n" + edge + " " + line)
		b.WriteString(strings.Repeat(" ", inner-width+1))
		b.WriteString(edge)
	}
	b.WriteString("\n" + s.border.Render("╰"+strings.Repeat("─", inner+2)+"╯"))
	return b.String()
}

func (s styles) grid(headers []string, rows [][]string, w int) string {
	widths := make([]int, len(headers))
	for _, row := range append([][]string{headers}, rows...) {
		for i, cell := range row {
			widths[i] = max(widths[i], lipgloss.Width(cell))
		}
	}
	total := 0
	for _, width := range widths {
		total += width + 2
	}
	var lines []string
	if total-2 <= w {
		for r, row := range append([][]string{headers}, rows...) {
			style := s.text
			if r == 0 {
				style = s.muted
			}
			cells := make([]string, len(row))
			for i, cell := range row {
				cells[i] = pad(style.Render(cell), widths[i])
			}
			lines = append(lines, strings.TrimRight(strings.Join(cells, "  "), " "))
		}
		return strings.Join(lines, "\n")
	}
	lines = append(lines, s.muted.Render(headers[0]))
	for _, row := range rows {
		var facts []string
		for i, cell := range row[1:] {
			if cell != "" {
				facts = append(facts, strings.TrimSpace(headers[i+1]+" "+cell))
			}
		}
		lines = append(lines, s.text.Render(row[0]))
		for _, line := range wrapParts(facts, w-2) {
			lines = append(lines, "  "+s.muted.Render(line))
		}
	}
	return strings.Join(lines, "\n")
}

type point struct {
	t, v float64
	peak float64
	n    int
}

const historyPoints, traceStep = 480, 0.05

type trace struct {
	points  []point
	step    float64
	version uint64
}

func (tr trace) add(t, v float64) trace {
	if math.IsInf(v, 0) || math.IsNaN(t) || math.IsInf(t, 0) {
		return tr
	}
	tr.version++
	tr.step = max(tr.step, traceStep)
	if n := len(tr.points); n > 0 {
		last := &tr.points[n-1]
		if t-last.t < tr.step && !math.IsNaN(v) && !math.IsNaN(last.v) {
			last.n++
			last.v += (v - last.v) / float64(last.n)
			last.peak = max(last.peak, v)
			return tr
		}
	}
	if len(tr.points) == historyPoints {
		tr.points, tr.step = coarsen(tr.points), tr.step*2
	}
	peak := 0.0
	if !math.IsNaN(v) {
		peak = max(v, 0)
	}
	tr.points = append(tr.points, point{t: t, v: v, peak: peak, n: 1})
	return tr
}

func coarsen(points []point) []point {
	out := points[:0]
	for i := 0; i < len(points); i += 2 {
		p := points[i]
		if i+1 < len(points) {
			q := points[i+1]
			p.peak = max(p.peak, q.peak)
			switch {
			case math.IsNaN(q.v):
				p.v = q.v
			case !math.IsNaN(p.v):
				p.v = (p.v*float64(p.n) + q.v*float64(q.n)) / float64(p.n+q.n)
				p.n += q.n
			}
		}
		out = append(out, p)
	}
	return out
}

type series struct {
	style  lipgloss.Style
	points []point
	dashed bool
}

type mark struct {
	t     float64
	stage goclient.Stage
}

func (s styles) stageSeries(points []point, marks []mark, direction ...goclient.Direction) []series {
	out := make([]series, len(marks))
	for i := len(marks) - 1; i >= 0; i-- {
		at, _ := slices.BinarySearchFunc(points, marks[i].t, func(p point, t float64) int { return cmp.Compare(p.t, t) })
		dashed := len(direction) > 0 && direction[0] == goclient.Up && marks[i].stage == goclient.StageBidirectional
		out[i], points = series{style: s.trace[marks[i].stage], points: points[at:], dashed: dashed}, points[:at]
	}
	return out
}

type axis struct {
	scale float64
	label func(float64) string
}

type chartKey struct {
	versions    [2]uint64
	directions  [2]goclient.Direction
	marks, w, h int
	span        float64
	dark        bool
	server      string
	stage       goclient.Stage
	finished    bool
}

type chartCache struct {
	key  chartKey
	view string
}

func (c *chartCache) render(key chartKey, draw func() string) string {
	if c.view == "" || c.key != key {
		c.key, c.view = key, draw()
	}
	return c.view
}

var (
	rateAxis = axis{8, func(bits float64) string {
		v, unit := rateTier(bits, 1)
		return roundLabel(v) + " " + unit
	}}
	msAxis = axis{1e-6, func(ms float64) string { return roundLabel(ms) + " ms" }}
)

func roundLabel(v float64) string { return strconv.FormatFloat(math.Round(v*1000)/1000, 'f', -1, 64) }

func niceCeil(v float64) float64 {
	if v <= 0 {
		return 1
	}
	decade := math.Pow(10, math.Floor(math.Log10(v)))
	for _, f := range []float64{1, 2, 2.5, 5} {
		if f*decade >= v {
			return f * decade
		}
	}
	return 10 * decade
}

var brailleDots = [4][2]rune{{0x01, 0x08}, {0x02, 0x10}, {0x04, 0x20}, {0x40, 0x80}}

const chartAxis = 11

func (s styles) chart(lines []series, marks []mark, ax axis, span float64, w, h int) string {
	cols, rows := max(w-chartAxis, 4), max(h-2, 2)
	t0, t1, peak := 0.0, max(span, 1), 0.0
	for _, l := range lines {
		for _, p := range l.points {
			peak = max(peak, p.peak)
			if !math.IsNaN(p.v) {
				peak = max(peak, p.v)
			}
		}
	}
	top := niceCeil(peak*ax.scale*1.05) / ax.scale
	dotW, dotH := cols*2, rows*4
	dots := make([]rune, cols*rows)
	owner := make([]int, cols*rows)
	set := func(x, y, i int) {
		if lines[i].dashed && x%6 >= 4 {
			return
		}
		cell := y/4*cols + x/2
		dots[cell] |= brailleDots[y%4][x%2]
		owner[cell] = i
	}
	segment := func(x0, y0, x1, y1, i int) {
		dx, dy, sx, sy := abs(x1-x0), -abs(y1-y0), cmp.Compare(x1, x0), cmp.Compare(y1, y0)
		for e := dx + dy; ; {
			set(x0, y0, i)
			if x0 == x1 && y0 == y1 {
				return
			}
			e2 := 2 * e
			if e2 >= dy {
				e, x0 = e+dy, x0+sx
			}
			if e2 <= dx {
				e, y0 = e+dx, y0+sy
			}
		}
	}
	for i, l := range lines {
		col, sum, n, px, py, drawn := -1, 0.0, 0, 0, 0, false
		plot := func() {
			if n == 0 {
				return
			}
			y := dotH - 1 - int(math.Round(min(max(sum/float64(n)/top, 0), 1)*float64(dotH-1)))
			if !drawn {
				px, py = col, y
			}
			segment(px, py, col, y, i)
			px, py, drawn, sum, n = col, y, true, 0, 0
		}
		for _, p := range l.points {
			if math.IsNaN(p.v) {
				plot()
				drawn = false
				continue
			}
			if x := min(max(int((p.t-t0)/(t1-t0)*float64(dotW)), 0), dotW-1); x != col {
				plot()
				col = x
			}
			sum, n = sum+p.v*float64(p.n), n+p.n
		}
		plot()
	}
	paints := make([][2]string, len(lines))
	for i, line := range lines {
		paints[i][0], paints[i][1], _ = strings.Cut(line.style.Render("x"), "x")
	}
	var b strings.Builder
	b.Grow(rows * (cols*3 + 80))
	for r := range rows {
		scale := ""
		switch r {
		case 0:
			if peak > 0 {
				scale = ax.label(top * ax.scale)
			}
		case rows - 1:
			scale = "0"
		case rows / 2:
			if peak > 0 && rows >= 6 {
				scale = ax.label(top * ax.scale / 2)
			}
		}
		scale = lipgloss.PlaceHorizontal(chartAxis-1, lipgloss.Right, ansi.Truncate(scale, chartAxis-1, ""))
		b.WriteString(s.muted.Render(scale) + s.border.Render("│"))
		for c := 0; c < cols; {
			first, end := r*cols+c, r*cols+c
			for end < (r+1)*cols && owner[end] == owner[first] && (dots[end] == 0) == (dots[first] == 0) {
				end++
			}
			if dots[first] == 0 {
				if r == rows/2 && rows >= 6 {
					b.WriteString(s.border.Render(strings.Repeat("┄", end-first)))
				} else {
					b.WriteString(strings.Repeat(" ", end-first))
				}
			} else {
				paint := paints[owner[first]]
				b.WriteString(paint[0])
				for k := first; k < end; k++ {
					b.WriteRune(0x2800 + dots[k])
				}
				b.WriteString(paint[1])
			}
			c += end - first
		}
		b.WriteString("\n")
	}
	ruler := []rune(strings.Repeat("─", cols))
	var labels strings.Builder
	at := 0
	end := ansi.Truncate(fmtClock(time.Duration(t1*float64(time.Second))), cols, "")
	endAt := cols - lipgloss.Width(end)
	column := func(t float64) int { return min(int((t-t0)/(t1-t0)*float64(cols)), cols-1) }
	for i, m := range marks {
		x, limit := column(m.t), endAt-1
		if x < 0 || x >= cols {
			continue
		}
		if i+1 < len(marks) {
			limit = min(limit, column(marks[i+1].t)-1)
		}
		ruler[x] = '┬'
		if room := limit - x; room >= 3 {
			label := ansi.Truncate(compactStage(m.stage), room, "…")
			labels.WriteString(strings.Repeat(" ", x-at) + s.stage[m.stage].Render(label))
			at = x + lipgloss.Width(label)
		}
	}
	b.WriteString(strings.Repeat(" ", chartAxis-1) + s.border.Render("└"+string(ruler)) + "\n")
	b.WriteString(strings.Repeat(" ", chartAxis) + labels.String() + strings.Repeat(" ", endAt-at) + s.muted.Render(end))
	return b.String()
}

func abs(n int) int { return max(n, -n) }
