package main

import (
	"image/color"

	"charm.land/bubbles/v2/help"
	"charm.land/lipgloss/v2"
	"github.com/zR-JB/graphite-meter/go/internal/goclient"
)

type styles struct {
	dark                                 bool
	title, pill, selected                lipgloss.Style
	text, value, muted, accent, ok, warn lipgloss.Style
	err, border, heading                 lipgloss.Style
	stage                                map[goclient.Stage]lipgloss.Style
	trace                                map[goclient.Stage]lipgloss.Style
	outcome                              map[goclient.Outcome]lipgloss.Style
	shades                               map[goclient.Stage][6]lipgloss.Style
	canvas                               color.Color
	plate, plateNote, plateOff           lipgloss.Style
}

// mix is share of c over the canvas, in sRGB.
func mix(c color.Color, canvas string, share float64) color.Color {
	r, g, b, _ := c.RGBA()
	under := lipgloss.Color(canvas)
	ur, ug, ub, _ := under.RGBA()
	blend := func(a, b uint32) uint8 { return uint8((float64(a>>8)*share + float64(b>>8)*(1-share)) + 0.5) }
	return color.RGBA{blend(r, ur), blend(g, ug), blend(b, ub), 0xff}
}

func newStyles(dark bool) styles {
	pick := lipgloss.LightDark(dark)
	tone := func(light, dark string) color.Color { return pick(lipgloss.Color(light), lipgloss.Color(dark)) }
	fg := func(c color.Color) lipgloss.Style { return lipgloss.NewStyle().Foreground(c) }
	ink, text, soft := tone("#20242a", "#e6e8eb"), tone("#171b20", "#eef0f3"), tone("#5f646a", "#8e9299")
	good, caution, bad := tone("#2e734b", "#88d1a2"), tone("#85671f", "#e8cf83"), tone("#ab413e", "#ed8b88")
	badge := lipgloss.NewStyle().Bold(true).Foreground(tone("#fdfdfd", "#0d1013")).Padding(0, 1)
	s := styles{
		dark:   dark,
		title:  badge.Background(ink),
		pill:   badge.Background(soft),
		text:   fg(text),
		muted:  fg(soft),
		accent: fg(ink),
		value:  fg(text).Bold(true),
		ok:     fg(good),
		warn:   fg(caution),
		err:    fg(bad).Bold(true),
		border: fg(tone("#cacbcf", "#3e4348")),
		stage: map[goclient.Stage]lipgloss.Style{
			goclient.StageLatency:       fg(tone("#1d7a73", "#70dbc4")),
			goclient.StageDownload:      fg(tone("#254ea3", "#71a3ff")),
			goclient.StageUpload:        fg(tone("#a35d1d", "#feb66a")),
			goclient.StageBidirectional: fg(tone("#7f2456", "#e472ac")),
		},
		trace: map[goclient.Stage]lipgloss.Style{
			goclient.StageLatency:       fg(tone("#0f9485", "#70dbc4")),
			goclient.StageDownload:      fg(tone("#275ac8", "#71a3ff")),
			goclient.StageUpload:        fg(tone("#ca6e03", "#feb66a")),
			goclient.StageBidirectional: fg(tone("#9b2065", "#e472ac")),
		},
		outcome: map[goclient.Outcome]lipgloss.Style{
			goclient.OutcomeComplete:   badge.Background(good),
			goclient.OutcomePartial:    badge.Background(caution),
			goclient.OutcomeIncomplete: badge.Background(caution),
			goclient.OutcomeStopped:    badge.Background(soft),
			goclient.OutcomeFailed:     badge.Background(bad),
		},
	}
	// A strip fades from its hue at the edge into the canvas below it.
	canvas := "#fdfdfd"
	if dark {
		canvas = "#0d1013"
	}
	s.canvas = lipgloss.Color(canvas)
	s.shades = map[goclient.Stage][6]lipgloss.Style{}
	for stage, trace := range s.trace {
		var shades [6]lipgloss.Style
		for i := range shades {
			shades[i] = fg(mix(trace.GetForeground(), canvas, 0.8-0.12*float64(i)))
		}
		s.shades[stage] = shades
	}
	s.plate = lipgloss.NewStyle().Foreground(lipgloss.Color(canvas)).Background(ink)
	s.plateNote = s.plate.Foreground(mix(ink, canvas, 0.4))
	s.plateOff = lipgloss.NewStyle().Foreground(soft).Background(tone("#dcdde0", "#2a2d31"))
	s.selected = s.text.Bold(true).Background(tone("#e6e6e9", "#303236"))
	s.heading = s.accent.Bold(true)
	return s
}

func (s styles) helpStyles() help.Styles {
	h := help.DefaultDarkStyles()
	h.ShortKey, h.FullKey = s.text, s.text
	h.ShortDesc, h.FullDesc = s.muted, s.muted
	h.ShortSeparator, h.FullSeparator, h.Ellipsis = s.border, s.border, s.border
	return h
}
