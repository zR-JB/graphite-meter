package main

import (
	"cmp"
	"image/color"
	"math"
	"slices"
	"strings"

	"github.com/charmbracelet/x/ansi"
)

// The dial: a 270° ring of braille dots opening downward, on the browser's transfer curve and scale, with nine ticks
// and five labels outside it and the readout inside it in figures drawn with heavy lines.
const (
	// cellW and cellH are a cell's width and height in pixels as most monospace fonts draw them; the ring is round at
	// this aspect.
	cellW, cellH = 9.0, 20.0
	dialStart    = 225.0
	dialTurn     = 270.0
	// dialMargin columns stay clear at each side for the labels.
	dialMargin = 6
	// The ring's shares of its radius: an arc's band, the track's and a head's bead.
	ringBand, ringTrack, ringBead = 0.08, 0.03, 0.16
)

// dialSmallest is the smallest area a ring is drawn in; a smaller dial shows only its readout.
var dialSmallest = [2]int{40, 14}

// arc is one arc of the dial: how far it reaches, 0–1 of the sweep, in its colour.
type arc struct {
	to    float64
	color color.Color
}

type dial struct {
	// arcs are each drawn to its end, the shorter over the longer so every head shows.
	arcs  []arc
	track color.Color
	ticks [5]string
	// label stands over the figures: the stage's mark and name; the note under the ring.
	label, note line
	value, unit string
}

func (p palette) drawDial(c *canvas, area rect, d dial) {
	if area.w < dialSmallest[0] || area.h < dialSmallest[1] {
		p.readout(c, area, d)
		return
	}
	w, h := float64(area.w-2*dialMargin)*cellW, float64(area.h-3)*cellH
	radius := min(w/2-8, (h-12)/(1+math.Sqrt2/2))
	// The label over the crown, the ring, and the note two rows under its ends stand centred in the area.
	tall := cellH + radius*(1+math.Sqrt2/2) + 16 + 2*cellH
	top := math.Max(math.Floor((float64(area.h)*cellH-tall)/2/cellH), 0) * cellH
	cx, cy := float64(area.w)*cellW/2, top+cellH+radius+8
	point := func(fraction, reach float64) (float64, float64) {
		angle := (dialStart - fraction*dialTurn) * math.Pi / 180
		return cx + reach*math.Cos(angle), cy - reach*math.Sin(angle)
	}
	band, track, bead := max(radius*ringBand, 9), max(radius*ringTrack, 4.5), max(radius*ringBead, 14)
	arcs := append([]arc(nil), d.arcs...)
	for i := range arcs {
		arcs[i].to = min(max(arcs[i].to, 0), 1)
	}
	slices.SortFunc(arcs, func(a, b arc) int { return cmp.Compare(a.to, b.to) })
	type head struct {
		x, y float64
		c    color.Color
	}
	heads := make([]head, len(arcs))
	for i, a := range arcs {
		x, y := point(a.to, radius)
		heads[i] = head{x, y, a.color}
	}
	type tick struct{ sin, cos float64 }
	ticks := make([]tick, 9)
	for i := range ticks {
		s, k := math.Sincos((dialStart - float64(i)/8*dialTurn) * math.Pi / 180)
		ticks[i] = tick{s, k}
	}
	ds := newDots(area)
	across, down := ds.size()
	for y := range down {
		for x := range across {
			px, py := (float64(x)+0.5)*cellW/2, (float64(y)+0.5)*cellH/4
			dx, dy := px-cx, cy-py
			distance := math.Hypot(dx, dy) - radius
			if math.Abs(distance) > max(bead, radius*0.2) {
				continue
			}
			fraction := math.Mod(dialStart-math.Atan2(dy, dx)*180/math.Pi+720, 360) / dialTurn
			var lit color.Color
			var rank uint8
			for _, hd := range heads {
				if math.Hypot(px-hd.x, py-hd.y) <= bead/2 {
					lit, rank = hd.c, 4
				}
			}
			if lit == nil {
				for _, a := range arcs {
					if fraction <= a.to && math.Abs(distance) <= band/2 {
						lit, rank = a.color, 3
						break
					}
				}
			}
			if lit == nil && fraction <= 1 && math.Abs(distance) <= track/2 {
				lit, rank = d.track, 1
			}
			if lit == nil {
				for i, t := range ticks {
					along, across := dx*t.cos+dy*t.sin, math.Abs(dy*t.cos-dx*t.sin)
					from := radius + band/2 + 4
					if along >= from && along <= from+radius*0.07 && across <= 2.4 {
						lit, rank = p.muted.fg, 2
						if i%2 == 1 {
							lit = p.border.fg
						}
						break
					}
				}
			}
			if lit != nil {
				ds.set(x, y, lit, rank)
			}
		}
	}
	c.dots(ds)
	// The labels sit beyond the ticks: the half's over the crown, the others beside their ticks.
	for i, label := range d.ticks {
		lx, ly := point(float64(i)/4, radius+band/2+radius*0.17+4)
		text := styled(label, p.muted)
		width := float64(text.width())
		column := lx/cellW - 0.5
		switch {
		case i == 2:
			column = lx/cellW - width/2
		case lx < cx:
			column = lx/cellW - width + 0.5
		}
		row := min(max(math.Floor(ly/cellH), 0), float64(area.h-2))
		column = min(max(math.Round(column), 0), float64(area.w)-width)
		c.text(area.x+int(column), area.y+int(row), int(width), text)
	}
	middle := area.y + int(cy/cellH)
	p.stack(c, rect{area.x, max(middle-3, 0), area.w, 7}, d)
	under := int(math.Ceil((cy+radius*math.Sqrt2/2)/cellH)) + 1
	c.center(area.x, min(area.y+under, area.bottom()-1), area.w, d.note)
}

// readout is the dial without its ring, centred in area: the label, the figures, the unit and the note.
func (p palette) readout(c *canvas, area rect, d dial) {
	top := area.y + max(area.h-7, 0)/2
	p.stack(c, rect{area.x, top, area.w, 7}, d)
	c.center(area.x, min(top+7, area.bottom()-1), area.w, d.note)
}

// stack is the label, the figures three rows tall and the unit, in seven rows.
func (p palette) stack(c *canvas, area rect, d dial) {
	c.center(area.x, area.y, area.w, d.label)
	rows := figures(d.value)
	width := 0
	for _, row := range rows {
		width = max(width, ansi.StringWidth(row))
	}
	st := p.text
	if d.value == missing {
		st = p.muted
	}
	for i, row := range rows {
		c.text(area.x+max(area.w-width, 0)/2, area.y+2+i, width, styled(row, st))
	}
	c.center(area.x, area.y+6, area.w, styled(d.unit, p.muted))
}

var figureGlyphs = map[rune][3]string{
	'0': {"┏━┓", "┃ ┃", "┗━┛"}, '1': {" ┓ ", " ┃ ", " ┻ "}, '2': {"╺━┓", "┏━┛", "┗━╸"},
	'3': {"╺━┓", " ━┫", "╺━┛"}, '4': {"╻ ╻", "┗━┫", "  ╹"}, '5': {"┏━╸", "┗━┓", "╺━┛"},
	'6': {"┏━╸", "┣━┓", "┗━┛"}, '7': {"╺━┓", "  ┃", "  ╹"}, '8': {"┏━┓", "┣━┫", "┗━┛"},
	'9': {"┏━┓", "┗━┫", "╺━┛"}, '.': {" ", " ", "•"}, '<': {" ", "<", " "},
	'—': {"   ", "╺━╸", "   "}, '-': {"   ", "╺━╸", "   "},
}

// figures is text in figures three rows tall drawn with heavy lines, a cell between them; characters without one are
// skipped, and the point hugs both its neighbours.
func figures(text string) [3]string {
	var rows [3]strings.Builder
	previous := rune(0)
	for _, r := range text {
		glyph, ok := figureGlyphs[r]
		if !ok {
			continue
		}
		gap := previous != 0 && previous != '.' && r != '.'
		for i, part := range glyph {
			if gap {
				rows[i].WriteByte(' ')
			}
			rows[i].WriteString(part)
		}
		previous = r
	}
	return [3]string{rows[0].String(), rows[1].String(), rows[2].String()}
}
