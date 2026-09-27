package main

import (
	"image/color"

	"charm.land/bubbles/v2/help"
	"charm.land/lipgloss/v2"
	"github.com/zR-JB/graphite-meter/go/internal/goclient"
)

type styles struct {
	title, pill, selected                lipgloss.Style
	text, value, muted, accent, ok, warn lipgloss.Style
	err, border, heading, down, up, rtt  lipgloss.Style
	outcome                              map[goclient.Outcome]lipgloss.Style
}

func newStyles(dark bool) styles {
	pick := lipgloss.LightDark(dark)
	tone := func(light, dark string) color.Color { return pick(lipgloss.Color(light), lipgloss.Color(dark)) }
	fg := func(c color.Color) lipgloss.Style { return lipgloss.NewStyle().Foreground(c) }
	brand, strong := tone("#2f717a", "#6db0b8"), tone("#235257", "#93cdd4")
	good, caution, bad := tone("#285443", "#79ad91"), tone("#6f5426", "#c4a568"), tone("#a04a4a", "#d89393")
	badge := lipgloss.NewStyle().Bold(true).Foreground(tone("#f6f5f1", "#111315")).Padding(0, 1)
	s := styles{
		title:  badge.Background(brand),
		pill:   badge.Background(strong),
		text:   fg(tone("#26272a", "#d9dce0")),
		muted:  fg(tone("#454a4d", "#9ba2aa")),
		accent: fg(brand),
		value:  fg(strong).Bold(true),
		ok:     fg(good),
		warn:   fg(caution),
		err:    fg(bad).Bold(true),
		border: fg(tone("#c3c3bf", "#3d4044")),
		outcome: map[goclient.Outcome]lipgloss.Style{
			goclient.OutcomeComplete:   badge.Background(good),
			goclient.OutcomePartial:    badge.Background(caution),
			goclient.OutcomeIncomplete: badge.Background(caution),
			goclient.OutcomeStopped:    badge.Background(caution),
			goclient.OutcomeFailed:     badge.Background(bad),
		},
	}
	s.selected = s.text.Bold(true).Background(tone("#eaeae4", "#23262b"))
	s.heading = s.accent.Bold(true)
	s.down, s.up, s.rtt = s.accent, fg(tone("#8f6425", "#bda36c")), s.ok
	return s
}

func (s styles) button(label string, focused bool) string {
	if focused {
		return s.title.Render(label)
	}
	return s.heading.Padding(0, 1).Render(label)
}

func (s styles) helpStyles() help.Styles {
	h := help.DefaultDarkStyles()
	h.ShortKey, h.FullKey = s.text, s.text
	h.ShortDesc, h.FullDesc = s.muted, s.muted
	h.ShortSeparator, h.FullSeparator, h.Ellipsis = s.border, s.border, s.border
	return h
}
