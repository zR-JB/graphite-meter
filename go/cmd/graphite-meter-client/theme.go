package main

import "github.com/charmbracelet/lipgloss"

// tone is one design token: its hex from client/src/app.css plus the 256- and
// 16-color stand-ins a poorer terminal falls back to. The stand-ins are chosen
// to hold both the token's hue and its WCAG AA contrast, which the nearest
// entry by distance does not always do.
type tone struct{ hex, x256, ansi string }

func adaptive(dark, light tone) lipgloss.CompleteAdaptiveColor {
	return lipgloss.CompleteAdaptiveColor{
		Dark:  lipgloss.CompleteColor{TrueColor: dark.hex, ANSI256: dark.x256, ANSI: dark.ansi},
		Light: lipgloss.CompleteColor{TrueColor: light.hex, ANSI256: light.x256, ANSI: light.ansi},
	}
}

// palette is the CARBON token set the web client is built from
// (client/src/app.css), which owns these values. Teal is the only accent;
// ok/warn/err carry status meaning and nothing else.
type palette struct {
	text        lipgloss.CompleteAdaptiveColor
	textMuted   lipgloss.CompleteAdaptiveColor
	textInverse lipgloss.CompleteAdaptiveColor
	brand       lipgloss.CompleteAdaptiveColor
	brandStrong lipgloss.CompleteAdaptiveColor
	surface     lipgloss.CompleteAdaptiveColor
	border      lipgloss.CompleteAdaptiveColor
	ok          lipgloss.CompleteAdaptiveColor
	warn        lipgloss.CompleteAdaptiveColor
	err         lipgloss.CompleteAdaptiveColor
}

// carbon carries the dark tokens for a dark terminal and the [data-theme=light]
// tokens for a light one. border is the web's translucent --border-strong
// flattened onto --canvas, since a terminal cell has no alpha.
var carbon = palette{
	text:        adaptive(tone{"#d9dce0", "253", "15"}, tone{"#26272a", "235", "0"}),
	textMuted:   adaptive(tone{"#9ba2aa", "247", "7"}, tone{"#454a4d", "239", "8"}),
	textInverse: adaptive(tone{"#111315", "233", "0"}, tone{"#f6f5f1", "255", "15"}),
	brand:       adaptive(tone{"#6db0b8", "73", "14"}, tone{"#2f717a", "23", "6"}),
	brandStrong: adaptive(tone{"#93cdd4", "116", "14"}, tone{"#235257", "23", "6"}),
	surface:     adaptive(tone{"#23262b", "235", "0"}, tone{"#eaeae4", "254", "7"}),
	border:      adaptive(tone{"#3d4044", "238", "8"}, tone{"#c3c3bf", "251", "7"}),
	ok:          adaptive(tone{"#79ad91", "108", "10"}, tone{"#285443", "22", "2"}),
	warn:        adaptive(tone{"#c4a568", "179", "11"}, tone{"#6f5426", "58", "3"}),
	err:         adaptive(tone{"#d89393", "174", "9"}, tone{"#a04a4a", "95", "1"}),
}

// Chips carry the inverse text tone, so the light-teal chip of a dark terminal
// reads dark-on-light and the dark-teal chip of a light one reads light-on-dark.
var (
	shellStyle      = lipgloss.NewStyle().Margin(1, shellMargin)
	titleStyle      = lipgloss.NewStyle().Bold(true).Foreground(carbon.textInverse).Background(carbon.brand).Padding(0, 1)
	pillStyle       = lipgloss.NewStyle().Bold(true).Foreground(carbon.textInverse).Background(carbon.brandStrong).Padding(0, 1)
	panelStyle      = lipgloss.NewStyle().Border(lipgloss.RoundedBorder()).BorderForeground(carbon.border).Padding(1, 2)
	activeTabStyle  = lipgloss.NewStyle().Bold(true).Foreground(carbon.textInverse).Background(carbon.brand).Padding(0, 1)
	tabStyle        = lipgloss.NewStyle().Foreground(carbon.textMuted).Padding(0, 1)
	selectedStyle   = lipgloss.NewStyle().Bold(true).Foreground(carbon.text).Background(carbon.surface)
	labelStyle      = lipgloss.NewStyle().Foreground(carbon.text)
	valueStyle      = lipgloss.NewStyle().Bold(true).Foreground(carbon.brandStrong)
	mutedStyle      = lipgloss.NewStyle().Foreground(carbon.textMuted)
	errorStyle      = lipgloss.NewStyle().Bold(true).Foreground(carbon.err)
	accentStyle     = lipgloss.NewStyle().Foreground(carbon.brand)
	warnStyle       = lipgloss.NewStyle().Foreground(carbon.warn)
	successStyle    = lipgloss.NewStyle().Foreground(carbon.ok)
	subtleRuleStyle = lipgloss.NewStyle().Foreground(carbon.border)
	codeStyle       = lipgloss.NewStyle().Bold(true).Foreground(carbon.text).Border(lipgloss.RoundedBorder()).BorderForeground(carbon.brand).Padding(0, 1)
)
