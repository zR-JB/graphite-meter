package main

import (
	"github.com/charmbracelet/bubbles/key"
	tea "github.com/charmbracelet/bubbletea"
)

// keymap holds every binding the program answers to. A screen publishes the
// subset it accepts through ShortHelp, which is what the footer renders, so a
// key that works is a key that is listed.
type keymap struct {
	sections key.Binding
	rows     key.Binding
	activate key.Binding
	run      key.Binding
	verify   key.Binding
	cancel   key.Binding
	confirm  key.Binding
	back     key.Binding
	rerun    key.Binding
	cursor   key.Binding
	apply    key.Binding
	discard  key.Binding
	help     key.Binding
	quit     key.Binding
	abort    key.Binding
}

var keys = keymap{
	sections: key.NewBinding(key.WithKeys("tab", "shift+tab", "right", "left"), key.WithHelp("tab/⇧tab/←/→", "section")),
	rows:     key.NewBinding(key.WithKeys("up", "down", "k", "j"), key.WithHelp("↑/↓/j/k", "row")),
	activate: key.NewBinding(key.WithKeys("enter", " "), key.WithHelp("enter/space", "open")),
	run:      key.NewBinding(key.WithKeys("r"), key.WithHelp("r", "run")),
	verify:   key.NewBinding(key.WithKeys("v"), key.WithHelp("v", "recheck")),
	cancel:   key.NewBinding(key.WithKeys("esc"), key.WithHelp("esc", "cancel")),
	confirm:  key.NewBinding(key.WithKeys("esc"), key.WithHelp("esc", "confirm cancel")),
	back:     key.NewBinding(key.WithKeys("esc"), key.WithHelp("esc", "setup")),
	rerun:    key.NewBinding(key.WithKeys("r"), key.WithHelp("r", "run again")),
	cursor:   key.NewBinding(key.WithKeys("left", "right", "home", "end"), key.WithHelp("←/→ home/end", "move")),
	apply:    key.NewBinding(key.WithKeys("enter"), key.WithHelp("enter", "apply")),
	discard:  key.NewBinding(key.WithKeys("esc"), key.WithHelp("esc", "discard")),
	help:     key.NewBinding(key.WithKeys("?"), key.WithHelp("?", "keys")),
	quit:     key.NewBinding(key.WithKeys("q", "ctrl+c"), key.WithHelp("q", "quit")),
	abort:    key.NewBinding(key.WithKeys("ctrl+c"), key.WithHelp("ctrl+c", "quit")),
}

// reverse reports whether msg is the backward half of a two-way binding.
func reverse(msg tea.KeyMsg) bool {
	switch msg.String() {
	case "shift+tab", "left", "up", "k":
		return true
	}
	return false
}

// ShortHelp is the footer for the screen on show: every binding it accepts.
func (m model) ShortHelp() []key.Binding {
	switch {
	case m.edit.kind != editNone:
		return []key.Binding{keys.cursor, keys.apply, keys.discard, keys.abort}
	case m.cancelPrompt:
		return []key.Binding{keys.confirm, keys.quit}
	case m.mode == modeRun && m.complete:
		return []key.Binding{keys.back, keys.rerun, keys.help, keys.quit}
	case m.mode == modeRun:
		return []key.Binding{keys.cancel, keys.help, keys.quit}
	default:
		return []key.Binding{keys.sections, keys.rows, keys.activate, keys.run, keys.verify, keys.help, keys.quit}
	}
}

// FullHelp lays the same bindings out in columns of three, so the expanded
// view can never disagree with the footer about what a screen accepts.
func (m model) FullHelp() [][]key.Binding {
	all := m.ShortHelp()
	cols := make([][]key.Binding, 0, (len(all)+2)/3)
	for i := 0; i < len(all); i += 3 {
		cols = append(cols, all[i:min(i+3, len(all))])
	}
	return cols
}
