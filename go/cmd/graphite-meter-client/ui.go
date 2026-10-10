package main

import (
	"math"
	"strings"

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

// panel is a section: its rule and title over the body, padded to h rows when h is set.
func (s styles) panel(title, body string, w, h int) string {
	lines := s.section(title, w)
	for line := range strings.SplitSeq(body, "\n") {
		lines = append(lines, fit(line, w))
	}
	for len(lines) < h {
		lines = append(lines, "")
	}
	return strings.Join(lines, "\n")
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

type mark struct {
	t     float64
	stage goclient.Stage
}

// niceCeil is the browser's chart step (client/src/lib/presentation/scales.ts) at or above v.
func niceCeil(v float64) float64 { return ceilStep(v, 1, 1.2, 1.5, 2, 2.5, 3, 4, 5, 6, 8) }
