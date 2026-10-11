package main

import (
	"image/color"
	"strings"
	"unicode"
	"unicode/utf8"

	uv "github.com/charmbracelet/ultraviolet"
	"github.com/charmbracelet/x/ansi"
	"github.com/zR-JB/graphite-meter/go/internal/goclient"
)

// span is text in one style, and line a row of them.
type (
	span struct {
		text string
		st   style
	}
	line []span
)

func styled(text string, st style) line { return line{{text, st}} }

func (l line) and(text string, st style) line { return append(l[:len(l):len(l)], span{text, st}) }

func (l line) with(other line) line { return append(l[:len(l):len(l)], other...) }

func (l line) width() int {
	w := 0
	for _, s := range l {
		w += ansi.StringWidth(s.text)
	}
	return w
}

func (l line) text() string {
	var b strings.Builder
	for _, s := range l {
		b.WriteString(s.text)
	}
	return b.String()
}

// fit cuts the line to w cells, a longer one ending in … in the style it cuts.
func (l line) fit(w int) line {
	w = max(w, 1)
	if l.width() <= w {
		return l
	}
	room, out := w-1, line{}
	for _, s := range l {
		if sw := ansi.StringWidth(s.text); sw <= room {
			out, room = append(out, s), room-sw
			continue
		}
		return append(out, span{ansi.Truncate(s.text, room, "") + "…", s.st})
	}
	return out
}

// rect is a region of cells.
type rect struct{ x, y, w, h int }

func (r rect) right() int  { return r.x + r.w }
func (r rect) bottom() int { return r.y + r.h }

func (r rect) contains(x, y int) bool { return x >= r.x && x < r.right() && y >= r.y && y < r.bottom() }

// target is what a press on a drawn region does.
type target struct {
	kind   targetKind
	stage  goclient.Stage
	action action
	index  int
}

type targetKind int

const (
	noTarget targetKind = iota
	// targetKey is the run key: start, or stop while a run goes.
	targetKey
	// targetChip switches its stage on or off between runs.
	targetChip
	// targetCap does what its key does.
	targetCap
	// targetServer opens the server's settings.
	targetServer
	targetRow
	targetEntry
	targetLink
	// targetRemember is the connect card's switch that remembers the server for later starts.
	targetRemember
	// targetSheet is a sheet's own ground; targetBackdrop, the dimmed console around it, closes it.
	targetSheet
	targetBackdrop
)

type hit struct {
	area   rect
	target target
}

// canvas is a frame being drawn into cells, and where its pressable regions landed, the topmost last.
type canvas struct {
	buf   *uv.Buffer
	tiles bool
	hits  []hit
}

// newCanvas is a w×h frame; tiles fill only when the terminal shows 256 colours or more.
func newCanvas(w, h int, tiles bool) *canvas {
	return &canvas{buf: uv.NewBuffer(w, h), tiles: tiles}
}

func (c *canvas) area() rect { return rect{0, 0, c.buf.Width(), c.buf.Height()} }

func (c *canvas) cell(x, y int) *uv.Cell { return c.buf.CellAt(x, y) }

func (c *canvas) each(r rect, f func(*uv.Cell)) {
	for y := max(r.y, 0); y < min(r.bottom(), c.buf.Height()); y++ {
		for x := max(r.x, 0); x < min(r.right(), c.buf.Width()); x++ {
			if cell := c.cell(x, y); cell != nil {
				f(cell)
			}
		}
	}
}

// fill paints a tile's colour under r, from 256 colours on; with fewer the canvas stays flat.
func (c *canvas) fill(r rect, col color.Color) {
	if c.tiles {
		c.plate(r, col)
	}
}

// plate paints col under r in every profile: a key's plate.
func (c *canvas) plate(r rect, col color.Color) {
	c.each(r, func(cell *uv.Cell) { cell.Style.Bg = col })
}

// clear blanks r, its colours with it.
func (c *canvas) clear(r rect) {
	c.each(r, func(cell *uv.Cell) { *cell = uv.EmptyCell })
}

// text writes l from column x of row y in at most w cells, every control and bidi override blanked; a span without a
// background keeps the one under it. It returns the column after the text.
func (c *canvas) text(x, y, w int, l line) int {
	if y < 0 || y >= c.buf.Height() {
		return x
	}
	end := min(x+w, c.buf.Width())
	for _, s := range l {
		for _, r := range s.text {
			if unicode.IsControl(r) || unicode.Is(unicode.Bidi_Control, r) || r == utf8.RuneError {
				r = ' '
			}
			cw := ansi.StringWidth(string(r))
			if cw == 0 || x+cw > end {
				if x+cw > end {
					return x
				}
				continue
			}
			if x >= 0 {
				cell := c.cell(x, y)
				bg := cell.Style.Bg
				if s.st.bg != nil {
					bg = s.st.bg
				}
				var attrs uint8
				if s.st.bold {
					attrs = uv.AttrBold
				}
				c.buf.SetCell(x, y, &uv.Cell{Content: string(r), Width: cw, Style: uv.Style{Fg: s.st.fg, Bg: bg, Attrs: attrs}})
			}
			x += cw
		}
	}
	return x
}

// right is l ending at the right edge of the w cells from x.
func (c *canvas) right(x, y, w int, l line) {
	shown := min(l.width(), w)
	c.text(x+w-shown, y, shown, l)
}

// center is l centred in the w cells from x.
func (c *canvas) center(x, y, w int, l line) {
	shown := min(l.width(), w)
	c.text(x+(w-shown)/2, y, shown, l)
}

// hit lets a press in r reach t.
func (c *canvas) hit(r rect, t target) { c.hits = append(c.hits, hit{r, t}) }

// dim mixes the colours under r toward the canvas by share, text in the terminal's own colour taking soft: the
// backdrop under a sheet.
func (c *canvas) dim(r rect, canvas, soft color.Color, share float64) {
	// Each colour mixes once: a mix boxes a new colour, and a backdrop holds few.
	dimmed := map[color.Color]color.Color{nil: nil}
	toward := func(col color.Color) color.Color {
		mixed, ok := dimmed[col]
		if !ok {
			mixed = mix(canvas, col, share)
			dimmed[col] = mixed
		}
		return mixed
	}
	c.each(r, func(cell *uv.Cell) {
		if cell.Style.Fg == nil {
			cell.Style.Fg = soft
		}
		cell.Style.Fg, cell.Style.Bg = toward(cell.Style.Fg), toward(cell.Style.Bg)
		cell.Style.Attrs &^= uv.AttrBold
	})
}

func (c *canvas) render() string { return c.buf.Render() }

// brailleBits is the dot of each row and column within a braille cell.
var brailleBits = [4][2]uint8{{0x01, 0x08}, {0x02, 0x10}, {0x04, 0x20}, {0x40, 0x80}}

// dots are braille dots over an area, two across and four down a cell; a cell shows its highest rank's dots in that
// rank's first colour, so a mark drawn over a trace stays whole.
type dots struct {
	area  rect
	cells []dotCell
}

type dotCell struct {
	mask  uint8
	color color.Color
	rank  uint8
}

func newDots(area rect) *dots {
	return &dots{area: area, cells: make([]dotCell, max(area.w*area.h, 0))}
}

// size is the dots across and down the area.
func (d *dots) size() (int, int) { return d.area.w * 2, d.area.h * 4 }

// set lights the dot x across and y down in col at rank; one off the area is dropped.
func (d *dots) set(x, y int, col color.Color, rank uint8) {
	across, down := d.size()
	if x < 0 || y < 0 || x >= across || y >= down {
		return
	}
	cell := &d.cells[y/4*d.area.w+x/2]
	if rank < cell.rank && cell.mask != 0 {
		return
	}
	if rank > cell.rank || cell.mask == 0 {
		*cell = dotCell{color: col, rank: rank}
	}
	cell.mask |= brailleBits[y%4][x%2]
}

// dots writes the cells d lit as braille in their colours, over the backgrounds under them.
func (c *canvas) dots(d *dots) {
	for i, dc := range d.cells {
		if dc.mask == 0 {
			continue
		}
		x, y := d.area.x+i%d.area.w, d.area.y+i/d.area.w
		if cell := c.cell(x, y); cell != nil {
			bg := cell.Style.Bg
			c.buf.SetCell(x, y, &uv.Cell{Content: string(rune(0x2800 + int(dc.mask))), Width: 1,
				Style: uv.Style{Fg: dc.color, Bg: bg}})
		}
	}
}
