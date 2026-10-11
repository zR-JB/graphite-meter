package main

import (
	"image/color"

	"charm.land/lipgloss/v2"
	"github.com/zR-JB/graphite-meter/go/internal/goclient"
)

// tone is a colour's light and dark shades; docs/DESIGN.md names the tokens they repeat.
type tone struct{ light, dark string }

var (
	inkTone     = tone{"#20242a", "#e6e8eb"}
	textTone    = tone{"#171b20", "#eef0f3"}
	softTone    = tone{"#5f646a", "#8e9299"}
	goodTone    = tone{"#2e734b", "#88d1a2"}
	cautionTone = tone{"#85671f", "#e8cf83"}
	badTone     = tone{"#ab413e", "#ed8b88"}
	borderTone  = tone{"#cacbcf", "#3e4348"}
	plateOff    = tone{"#dcdde0", "#2a2d31"}
	// canvasTone is the page when the terminal does not name its background.
	canvasTone = tone{"#fdfdfd", "#0d1013"}
	stageTones = map[goclient.Stage]tone{
		goclient.StageLatency:       {"#1d7a73", "#70dbc4"},
		goclient.StageDownload:      {"#254ea3", "#71a3ff"},
		goclient.StageUpload:        {"#a35d1d", "#feb66a"},
		goclient.StageBidirectional: {"#7f2456", "#e472ac"},
	}
	traceTones = map[goclient.Stage]tone{
		goclient.StageLatency:       {"#0f9485", "#70dbc4"},
		goclient.StageDownload:      {"#275ac8", "#71a3ff"},
		goclient.StageUpload:        {"#ca6e03", "#feb66a"},
		goclient.StageBidirectional: {"#9b2065", "#e472ac"},
	}
)

func (t tone) pick(dark bool) color.Color {
	if dark {
		return lipgloss.Color(t.dark)
	}
	return lipgloss.Color(t.light)
}

// mix is share of c over under, in sRGB.
func mix(c, under color.Color, share float64) color.Color {
	r, g, b, _ := c.RGBA()
	ur, ug, ub, _ := under.RGBA()
	blend := func(a, b uint32) uint8 { return uint8(float64(a>>8)*share + float64(b>>8)*(1-share) + 0.5) }
	return color.RGBA{blend(r, ur), blend(g, ug), blend(b, ub), 0xff}
}

// styles are the printed report's, in lipgloss.
type styles struct {
	text, value, muted, heading, warn, err lipgloss.Style
	stage                                  map[goclient.Stage]lipgloss.Style
	outcome                                map[goclient.Outcome]lipgloss.Style
}

func newStyles(dark bool) styles {
	fg := func(t tone) lipgloss.Style { return lipgloss.NewStyle().Foreground(t.pick(dark)) }
	badge := lipgloss.NewStyle().Bold(true).Foreground(canvasTone.pick(dark)).Padding(0, 1)
	s := styles{
		text:    fg(textTone),
		value:   fg(textTone).Bold(true),
		muted:   fg(softTone),
		heading: fg(inkTone).Bold(true),
		warn:    fg(cautionTone),
		err:     fg(badTone).Bold(true),
		stage:   map[goclient.Stage]lipgloss.Style{},
		outcome: map[goclient.Outcome]lipgloss.Style{
			goclient.OutcomeComplete:   badge.Background(goodTone.pick(dark)),
			goclient.OutcomePartial:    badge.Background(cautionTone.pick(dark)),
			goclient.OutcomeIncomplete: badge.Background(cautionTone.pick(dark)),
			goclient.OutcomeStopped:    badge.Background(softTone.pick(dark)),
			goclient.OutcomeFailed:     badge.Background(badTone.pick(dark)),
		},
	}
	for stage, t := range stageTones {
		s.stage[stage] = fg(t)
	}
	return s
}

// style is how a span of the console draws: a missing foreground is the terminal's, a missing background the one
// under it.
type style struct {
	fg, bg color.Color
	bold   bool
}

func fgStyle(c color.Color) style { return style{fg: c} }

func (s style) withBold() style { s.bold = true; return s }

func (s style) on(bg color.Color) style { s.bg = bg; return s }

// palette is the console's: styles, and tiles, plates and washes over the terminal's own background.
type palette struct {
	dark                                                       bool
	canvas                                                     color.Color
	text, value, muted, accent, heading, ok, warn, err, border style
	cursor, plate, plateNote, plateOff                         style
	// surface lifts tiles one step over the canvas and raised plates and caps two; hover and selected wash rows.
	surface, raised, hover, selected color.Color
}

// newPalette is the palette over canvas, or over the default canvas when the terminal did not name its background.
func newPalette(dark bool, canvas color.Color) palette {
	if canvas == nil {
		canvas = canvasTone.pick(dark)
	}
	fg := func(t tone) style { return fgStyle(t.pick(dark)) }
	share := func(darkShare, lightShare float64) color.Color {
		if dark {
			return mix(textTone.pick(true), canvas, darkShare)
		}
		return mix(textTone.pick(false), canvas, lightShare)
	}
	ink := inkTone.pick(dark)
	return palette{
		dark:      dark,
		canvas:    canvas,
		text:      fg(textTone),
		value:     fg(textTone).withBold(),
		muted:     fg(softTone),
		accent:    fg(inkTone),
		heading:   fg(inkTone).withBold(),
		ok:        fg(goodTone),
		warn:      fg(cautionTone),
		err:       fg(badTone).withBold(),
		border:    fg(borderTone),
		cursor:    style{fg: canvas, bg: textTone.pick(dark)},
		plate:     style{fg: canvas, bg: ink},
		plateNote: style{fg: mix(ink, canvas, 0.45), bg: ink},
		plateOff:  style{fg: softTone.pick(dark), bg: plateOff.pick(dark)},
		surface:   share(0.045, 0.03),
		raised:    share(0.09, 0.07),
		hover:     share(0.07, 0.05),
		selected:  share(0.13, 0.1),
	}
}

func (p palette) stage(stage goclient.Stage) style { return fgStyle(stageTones[stage].pick(p.dark)) }

// trace is a stage's chart colour.
func (p palette) trace(stage goclient.Stage) color.Color { return traceTones[stage].pick(p.dark) }

// wash is share of a stage's hue over under: a tile's wash, a chart's field, a box plot's box.
func (p palette) wash(stage goclient.Stage, share float64, under color.Color) color.Color {
	return mix(p.trace(stage), under, share)
}

// outcome is an outcome's label: bold in its badge's colour.
func (p palette) outcome(o goclient.Outcome) style {
	t := map[goclient.Outcome]tone{goclient.OutcomeComplete: goodTone, goclient.OutcomePartial: cautionTone,
		goclient.OutcomeIncomplete: cautionTone, goclient.OutcomeStopped: softTone, goclient.OutcomeFailed: badTone}[o]
	return fgStyle(t.pick(p.dark)).withBold()
}
