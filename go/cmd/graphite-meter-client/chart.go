package main

import (
	"image/color"
	"math"
)

// A stage's traces in braille over its window: the rate as a line, and the replies as one bar a cell from the fastest
// to the slowest, ending in an arrowhead where a reply passes the top.

// liveHead is where a measuring stage's trace stands now: its time on the run's clock and its value.
type liveHead struct{ at, value float64 }

// traceLine draws points over [t0, t1] seconds as a line of dots up to head; a gap breaks it. The line's tip is lit in
// tip; with a field, the area under the line fills with its hue fading down into the field, a cell at a time under
// the line's exact dots.
func traceLine(c *canvas, area rect, points []point, head *liveHead, top, t0, t1 float64, col, tip, field color.Color) {
	ds := newDots(area)
	across, down := ds.size()
	span := max(t1-t0, 1e-9)
	height := func(v float64) int {
		share := min(max(v/top, 0), 1)
		return down - 1 - int(math.Round(share*float64(down-1)))
	}
	last := math.Inf(-1)
	if len(points) > 0 {
		last = points[len(points)-1].t
	}
	if head != nil && head.at <= last {
		head = nil
	}
	crests := make([]int, area.w)
	for i := range crests {
		crests[i] = -1
	}
	next, previous, tipX, tipY := 0, -1, -1, 0
	for x := range across {
		at := t0 + (float64(x)+0.5)/float64(across)*span
		for next < len(points) && points[next].t <= at {
			next++
		}
		value, ok := 0.0, false
		if next > 0 {
			before := points[next-1]
			switch {
			case math.IsNaN(before.v):
			case next < len(points):
				value, ok = between(before, points[next].t, points[next].v, at), true
			case head != nil && at <= head.at:
				value, ok = between(before, head.at, head.value, at), true
			case at <= last+span/float64(across):
				value, ok = before.v, true
			}
		}
		if !ok {
			previous = -1
			continue
		}
		y := height(value)
		if crest := &crests[x/2]; *crest < 0 || y < *crest {
			*crest = y
		}
		// A steep step fills the column between, so the line stays whole.
		from := y
		if previous >= 0 && abs(previous-y) > 1 {
			from = previous
		}
		for dot := min(from, y); dot <= max(from, y); dot++ {
			ds.set(x, dot, col, 1)
		}
		previous, tipX, tipY = y, x, y
	}
	if head != nil && tipX >= 0 {
		ds.set(tipX, tipY, tip, 2)
	}
	if field != nil {
		// The area starts in the line's own cell when the line runs in its upper half, else in the cell under it.
		for column, crest := range crests {
			if crest < 0 {
				continue
			}
			first := crest / 4
			if crest%4 > 1 {
				first++
			}
			for row := first; row < area.h; row++ {
				share := 0.05 + 0.2*(1-float64(row)/float64(area.h))
				c.fill(rect{area.x + column, area.y + row, 1, 1}, mix(col, field, share))
			}
		}
	}
	c.dots(ds)
}

// between is the value at `at` on the line from before to a later point at afterAt.
func between(before point, afterAt, after, at float64) float64 {
	if math.IsNaN(after) || afterAt <= before.t {
		return before.v
	}
	return before.v + (after-before.v)*min(max((at-before.t)/(afterAt-before.t), 0), 1)
}

// bars draws points over [t0, t1] seconds as one bar a cell from the fastest to the slowest value it holds, on a
// scale to top; a bar past it ends at the top in an arrowhead.
func bars(c *canvas, area rect, points []point, top, t0, t1 float64, col color.Color) {
	ds := newDots(area)
	_, down := ds.size()
	span := max(t1-t0, 1e-9)
	type extent struct {
		low, high float64
		seen      bool
	}
	ranges := make([]extent, area.w)
	for _, pt := range points {
		if math.IsNaN(pt.v) {
			continue
		}
		cell := int(math.Floor(max((pt.t-t0)/span*float64(area.w), 0)))
		if cell >= area.w {
			continue
		}
		r := &ranges[cell]
		if !r.seen {
			*r = extent{pt.low, pt.peak, true}
			continue
		}
		r.low, r.high = min(r.low, pt.low), max(r.high, pt.peak)
	}
	row := func(v float64) int { return down - 1 - int(math.Round(min(max(v/top, 0), 1)*float64(down-1))) }
	var over []int
	for cell, r := range ranges {
		if !r.seen {
			continue
		}
		for dot := row(r.high); dot <= row(r.low); dot++ {
			ds.set(2*cell, dot, col, 1)
		}
		if r.high > top {
			over = append(over, cell)
		}
	}
	c.dots(ds)
	for _, cell := range over {
		c.text(area.x+cell, area.y, 1, styled("▴", fgStyle(col)))
	}
}

func abs(n int) int { return max(n, -n) }
