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
	err, border, heading                 lipgloss.Style
	stage                                map[goclient.Stage]lipgloss.Style
	outcome                              map[goclient.Outcome]lipgloss.Style
}

func newStyles(dark bool) styles {
	pick := lipgloss.LightDark(dark)
	tone := func(light, dark string) color.Color { return pick(lipgloss.Color(light), lipgloss.Color(dark)) }
	fg := func(c color.Color) lipgloss.Style { return lipgloss.NewStyle().Foreground(c) }
	ink, text, soft := tone("#20242a", "#e6e8eb"), tone("#171b20", "#eef0f3"), tone("#5f646a", "#8e9299")
	good, caution, bad := tone("#2e734b", "#88d1a2"), tone("#85671f", "#e8cf83"), tone("#ab413e", "#ed8b88")
	badge := lipgloss.NewStyle().Bold(true).Foreground(tone("#fdfdfd", "#0d1013")).Padding(0, 1)
	s := styles{
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
			goclient.StageLatency:       fg(tone("#1d7a6f", "#70dbc4")),
			goclient.StageDownload:      fg(tone("#254ea3", "#71a3ff")),
			goclient.StageUpload:        fg(tone("#a35d1d", "#feb66a")),
			goclient.StageBidirectional: fg(tone("#7f2456", "#e472ac")),
		},
		outcome: map[goclient.Outcome]lipgloss.Style{
			goclient.OutcomeComplete:   badge.Background(good),
			goclient.OutcomePartial:    badge.Background(caution),
			goclient.OutcomeIncomplete: badge.Background(caution),
			goclient.OutcomeStopped:    badge.Background(caution),
			goclient.OutcomeFailed:     badge.Background(bad),
		},
	}
	s.selected = s.text.Bold(true).Background(tone("#e6e6e9", "#2d2f33"))
	s.heading = s.accent.Bold(true)
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
