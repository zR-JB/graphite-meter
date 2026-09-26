package main

import (
	"charm.land/bubbles/v2/help"
	"charm.land/lipgloss/v2"
)

type styles struct {
	title, pill, tab, activeTab, selected lipgloss.Style
	text, value, muted, accent, ok, warn  lipgloss.Style
	err, border, heading, down, up, rtt   lipgloss.Style
}

func newStyles(dark bool) styles {
	pick := lipgloss.LightDark(dark)
	hex := func(light, dark string) lipgloss.Style {
		return lipgloss.NewStyle().Foreground(pick(lipgloss.Color(light), lipgloss.Color(dark)))
	}
	inverse := pick(lipgloss.Color("#f6f5f1"), lipgloss.Color("#111315"))
	brand := pick(lipgloss.Color("#2f717a"), lipgloss.Color("#6db0b8"))
	strong := pick(lipgloss.Color("#235257"), lipgloss.Color("#93cdd4"))
	badge := lipgloss.NewStyle().Bold(true).Foreground(inverse).Padding(0, 1)
	s := styles{
		title:     badge.Background(brand),
		pill:      badge.Background(strong),
		activeTab: badge.Background(brand),
		text:      hex("#26272a", "#d9dce0"),
		muted:     hex("#454a4d", "#9ba2aa"),
		accent:    lipgloss.NewStyle().Foreground(brand),
		value:     lipgloss.NewStyle().Bold(true).Foreground(strong),
		ok:        hex("#285443", "#79ad91"),
		warn:      hex("#6f5426", "#c4a568"),
		err:       hex("#a04a4a", "#d89393").Bold(true),
		border:    hex("#c3c3bf", "#3d4044"),
	}
	s.tab = s.muted.Padding(0, 1)
	s.selected = s.text.Bold(true).Background(pick(lipgloss.Color("#eaeae4"), lipgloss.Color("#23262b")))
	s.heading = s.accent.Bold(true)
	s.down, s.up, s.rtt = s.accent, s.value.UnsetBold(), s.ok
	return s
}

func (s styles) helpStyles() help.Styles {
	h := help.DefaultDarkStyles()
	h.ShortKey, h.FullKey = s.text, s.text
	h.ShortDesc, h.FullDesc = s.muted, s.muted
	h.ShortSeparator, h.FullSeparator, h.Ellipsis = s.border, s.border, s.border
	return h
}
