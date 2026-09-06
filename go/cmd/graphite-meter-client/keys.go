package main

import (
	"slices"

	"github.com/charmbracelet/bubbles/key"
	tea "github.com/charmbracelet/bubbletea"
)

type keymap struct {
	servers, automatic, latencyFocus, toggleServer, serverDetails key.Binding
	sections                                                      key.Binding
	rows                                                          key.Binding
	activate                                                      key.Binding
	approve                                                       key.Binding
	run                                                           key.Binding
	verify                                                        key.Binding
	cancel                                                        key.Binding
	confirm                                                       key.Binding
	back                                                          key.Binding
	rerun                                                         key.Binding
	cursor                                                        key.Binding
	apply                                                         key.Binding
	discard                                                       key.Binding
	help                                                          key.Binding
	quit                                                          key.Binding
	abort                                                         key.Binding
}

var keys = keymap{
	serverDetails: key.NewBinding(key.WithKeys("d"), key.WithHelp("d", "server details")),
	servers:       key.NewBinding(key.WithKeys("s"), key.WithHelp("s", "servers")),
	automatic:     key.NewBinding(key.WithKeys("a"), key.WithHelp("a", "use automatic paths")),
	latencyFocus:  key.NewBinding(key.WithKeys("l"), key.WithHelp("l", "latency server")),
	toggleServer:  key.NewBinding(key.WithKeys(" "), key.WithHelp("space", "toggle server")),
	sections:      key.NewBinding(key.WithKeys("tab", "shift+tab", "right", "left"), key.WithHelp("tab/⇧tab/←/→", "section")),
	rows:          key.NewBinding(key.WithKeys("up", "down", "k", "j"), key.WithHelp("↑/↓/j/k", "row")),
	activate:      key.NewBinding(key.WithKeys("enter", " "), key.WithHelp("enter/space", "open")),
	approve:       key.NewBinding(key.WithKeys("enter", " ", "o"), key.WithHelp("enter", "open the approval page")),
	run:           key.NewBinding(key.WithKeys("r"), key.WithHelp("r", "run")),
	verify:        key.NewBinding(key.WithKeys("v"), key.WithHelp("v", "recheck")),
	cancel:        key.NewBinding(key.WithKeys("esc"), key.WithHelp("esc", "cancel")),
	confirm:       key.NewBinding(key.WithKeys("esc"), key.WithHelp("esc", "confirm cancel")),
	back:          key.NewBinding(key.WithKeys("esc"), key.WithHelp("esc", "setup")),
	rerun:         key.NewBinding(key.WithKeys("r"), key.WithHelp("r", "run again")),
	cursor:        key.NewBinding(key.WithKeys("left", "right", "home", "end"), key.WithHelp("←/→ home/end", "move")),
	apply:         key.NewBinding(key.WithKeys("enter"), key.WithHelp("enter", "apply")),
	discard:       key.NewBinding(key.WithKeys("esc"), key.WithHelp("esc", "discard")),
	help:          key.NewBinding(key.WithKeys("?"), key.WithHelp("?", "keys")),
	quit:          key.NewBinding(key.WithKeys("q", "ctrl+c"), key.WithHelp("q", "quit")),
	abort:         key.NewBinding(key.WithKeys("ctrl+c"), key.WithHelp("ctrl+c", "quit")),
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
	return slices.DeleteFunc(m.contextHelp(), func(binding key.Binding) bool {
		keys := binding.Keys()
		return len(keys) == 1 && ((keys[0] == "s" && !m.canChooseServers()) || ((keys[0] == "l" || keys[0] == "d") && !m.hasServerBreakdown()))
	})
}
func (m model) preparationHelp() string {
	if m.canChooseServers() {
		return "v retries · s servers · a Use Automatic"
	}
	return "v retries · a Use Automatic"
}
func (m model) contextHelp() []key.Binding {
	switch {
	case m.serverDetailsOpen:
		return []key.Binding{keys.rows, keys.discard, keys.quit}
	case m.serverChooser:
		return []key.Binding{keys.rows, keys.toggleServer, keys.apply, keys.discard, keys.quit}
	case m.edit.kind != editNone:
		return []key.Binding{keys.cursor, keys.apply, keys.discard, keys.abort}
	case m.cancelPrompt:
		return []key.Binding{keys.confirm, keys.quit}
	case m.mode == modeRun && m.complete:
		return []key.Binding{keys.back, keys.rerun, keys.latencyFocus, keys.serverDetails, keys.help, keys.quit}
	case m.mode == modeRun:
		return []key.Binding{keys.cancel, keys.latencyFocus, keys.serverDetails, keys.help, keys.quit}
	case m.auth != nil && !m.authOpened:
		// enter belongs to the approval until the page is opened, so the row it would otherwise activate is not offered.
		return []key.Binding{keys.approve, keys.sections, keys.rows, keys.run, keys.verify, keys.servers, keys.automatic, keys.help, keys.quit}
	default:
		return []key.Binding{keys.sections, keys.rows, keys.activate, keys.run, keys.verify, keys.servers, keys.automatic, keys.help, keys.quit}
	}
}

func (m model) FullHelp() [][]key.Binding {
	all := m.ShortHelp()
	cols := make([][]key.Binding, 0, (len(all)+2)/3)
	for i := 0; i < len(all); i += 3 {
		cols = append(cols, all[i:min(i+3, len(all))])
	}
	return cols
}
