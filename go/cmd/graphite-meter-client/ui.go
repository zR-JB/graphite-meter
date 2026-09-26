package main

import (
	"math"
	"strings"
	"time"

	"charm.land/lipgloss/v2"
	"github.com/charmbracelet/x/ansi"
)

func fit(s string, w int) string {
	lines := strings.Split(s, "\n")
	for i, line := range lines {
		lines[i] = ansi.Truncate(line, max(w, 1), "…")
	}
	return strings.Join(lines, "\n")
}

func clip(s string, h int) string {
	lines := strings.Split(s, "\n")
	return strings.Join(lines[:min(len(lines), max(h, 1))], "\n")
}

func (s styles) panel(title, body string, w, h int) string {
	inner := max(w-4, 1)
	box := lipgloss.NewStyle().Border(lipgloss.RoundedBorder(), false, true, true).
		BorderForeground(s.border.GetForeground()).Padding(0, 1).Width(w)
	if h > 0 {
		body = clip(body, h-2)
		box = box.Height(h - 1)
	}
	title = ansi.Truncate(title, max(w-6, 1), "…")
	fill := max(w-5-lipgloss.Width(title), 0)
	top := s.border.Render("╭─ ") + s.heading.Render(title) + s.border.Render(" "+strings.Repeat("─", fill)+"╮")
	return top + "\n" + box.Render(fit(body, inner))
}

func overlay(base, box string, w, top, h int) (string, int, int) {
	x := max((w-lipgloss.Width(box))/2, 0)
	y := top + max((h-lipgloss.Height(box))/2, 0)
	layers := lipgloss.NewCompositor(lipgloss.NewLayer(base), lipgloss.NewLayer(box).X(x).Y(y).Z(1))
	return layers.Render(), x, y
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
				cells[i] = pad(cell, widths[i])
			}
			lines = append(lines, strings.TrimRight(style.Render(strings.Join(cells, "  ")), " "))
		}
		return strings.Join(lines, "\n")
	}
	for _, row := range rows {
		var facts []string
		for i, cell := range row[1:] {
			if cell != "" {
				facts = append(facts, headers[i+1]+" "+cell)
			}
		}
		lines = append(lines, s.text.Render(row[0]))
		for _, line := range wrapParts(facts, w-2) {
			lines = append(lines, "  "+s.muted.Render(line))
		}
	}
	return strings.Join(lines, "\n")
}

type point struct{ t, v float64 }

type series struct {
	style  lipgloss.Style
	points []point
}

type mark struct {
	t     float64
	label string
}

var brailleDots = [4][2]rune{{0x01, 0x08}, {0x02, 0x10}, {0x04, 0x20}, {0x40, 0x80}}

// chart draws each series as a braille line; NaN values leave gaps, never interpolated data.
func (s styles) chart(lines []series, marks []mark, label func(float64) string, span float64, w, h int) string {
	const axis = 13
	cols, rows := max(w-axis, 4), max(h-2, 2)
	t0, t1, top := 0.0, max(span, 1), 0.0
	for _, l := range lines {
		for _, p := range l.points {
			if !math.IsNaN(p.v) {
				top = max(top, p.v)
			}
		}
	}
	top = max(top*1.1, 1e-9)
	dots := make([]rune, cols*rows)
	owner := make([]int, cols*rows)
	set := func(x, y, i int) {
		if x >= 0 && x < cols*2 && y >= 0 && y < rows*4 {
			cell := y/4*cols + x/2
			dots[cell] |= brailleDots[y%4][x%2]
			owner[cell] = i
		}
	}
	for i, l := range lines {
		px, py, drawn := 0, 0, false
		for _, p := range l.points {
			if math.IsNaN(p.v) {
				drawn = false
				continue
			}
			x := int((p.t - t0) / (t1 - t0) * float64(cols*2-1))
			y := rows*4 - 1 - int(p.v/top*float64(rows*4-1))
			if !drawn {
				px, py = x, y
			}
			steps := max(abs(x-px), abs(y-py), 1)
			for j := 0; j <= steps; j++ {
				set(px+(x-px)*j/steps, py+(y-py)*j/steps, i)
			}
			px, py, drawn = x, y, true
		}
	}
	var b strings.Builder
	for r := range rows {
		scale := ""
		switch r {
		case 0:
			scale = label(top)
		case rows - 1:
			scale = label(0)
		}
		scale = lipgloss.PlaceHorizontal(axis-1, lipgloss.Right, ansi.Truncate(scale, axis-1, ""))
		b.WriteString(s.muted.Render(scale) + s.border.Render("│"))
		for c := 0; c < cols; {
			first, end := r*cols+c, r*cols+c
			for end < (r+1)*cols && owner[end] == owner[first] && (dots[end] == 0) == (dots[first] == 0) {
				end++
			}
			if dots[first] == 0 {
				b.WriteString(strings.Repeat(" ", end-first))
			} else {
				glyphs := make([]rune, end-first)
				for k := range glyphs {
					glyphs[k] = 0x2800 + dots[first+k]
				}
				b.WriteString(lines[owner[first]].style.Render(string(glyphs)))
			}
			c += end - first
		}
		b.WriteString("\n")
	}
	ruler := []rune(strings.Repeat("─", cols))
	labels := []rune(strings.Repeat(" ", cols))
	for _, m := range marks {
		x := int((m.t - t0) / (t1 - t0) * float64(cols-1))
		if x >= 0 && x < cols {
			ruler[x] = '┬'
			copy(labels[x:], []rune(m.label))
		}
	}
	end := []rune(fmtClock(time.Duration(t1 * float64(time.Second))))
	copy(labels[max(cols-len(end), 0):], end)
	b.WriteString(strings.Repeat(" ", axis-1) + s.border.Render("└"+string(ruler)) + "\n")
	b.WriteString(strings.Repeat(" ", axis) + s.muted.Render(string(labels)))
	return b.String()
}

func abs(n int) int { return max(n, -n) }
