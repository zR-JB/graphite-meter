package main

import (
	"charm.land/bubbles/v2/key"
	tea "charm.land/bubbletea/v2"
)

type keymap struct {
	sections, rows, change, start, recheck, servers, automatic, available key.Binding
	openSignIn, cancelSignIn                                              key.Binding
	stop, confirmStop, setup, runAgain, latencyServer, details, scroll    key.Binding
	page, close                                                           key.Binding
	toggleServer, apply, discard, cursor, help, quit, abort               key.Binding
}

var keys = keymap{
	sections: key.NewBinding(
		key.WithKeys("tab", "shift+tab", "right", "left"),
		key.WithHelp("tab/←/→", "section"),
	),
	rows:          key.NewBinding(key.WithKeys("up", "down", "k", "j"), key.WithHelp("↑/↓", "row")),
	change:        key.NewBinding(key.WithKeys("enter", "space"), key.WithHelp("enter", "change")),
	start:         key.NewBinding(key.WithKeys("r"), key.WithHelp("r", "start test")),
	recheck:       key.NewBinding(key.WithKeys("v"), key.WithHelp("v", "recheck paths")),
	servers:       key.NewBinding(key.WithKeys("s"), key.WithHelp("s", "test servers")),
	automatic:     key.NewBinding(key.WithKeys("a"), key.WithHelp("a", "automatic paths")),
	available:     key.NewBinding(key.WithKeys("u"), key.WithHelp("u", "use available servers")),
	openSignIn:    key.NewBinding(key.WithKeys("enter", "space", "o"), key.WithHelp("enter", "open page")),
	cancelSignIn:  key.NewBinding(key.WithKeys("esc"), key.WithHelp("esc", "cancel")),
	stop:          key.NewBinding(key.WithKeys("esc"), key.WithHelp("esc", "stop test")),
	confirmStop:   key.NewBinding(key.WithKeys("esc"), key.WithHelp("esc", "confirm stop")),
	setup:         key.NewBinding(key.WithKeys("esc"), key.WithHelp("esc", "setup")),
	runAgain:      key.NewBinding(key.WithKeys("r"), key.WithHelp("r", "run again")),
	latencyServer: key.NewBinding(key.WithKeys("l"), key.WithHelp("l", "latency server")),
	details:       key.NewBinding(key.WithKeys("d"), key.WithHelp("d", "details")),
	scroll:        key.NewBinding(key.WithKeys("up", "down", "k", "j"), key.WithHelp("↑/↓", "scroll")),
	page:          key.NewBinding(key.WithKeys("pgup", "pgdown", "home", "end"), key.WithHelp("pgdn", "more")),
	close:         key.NewBinding(key.WithKeys("esc"), key.WithHelp("esc", "close")),
	toggleServer:  key.NewBinding(key.WithKeys("space"), key.WithHelp("space", "select")),
	apply:         key.NewBinding(key.WithKeys("enter"), key.WithHelp("enter", "apply")),
	discard:       key.NewBinding(key.WithKeys("esc"), key.WithHelp("esc", "cancel")),
	cursor:        key.NewBinding(key.WithKeys("left", "right", "home", "end"), key.WithHelp("←/→", "move")),
	help:          key.NewBinding(key.WithKeys("?"), key.WithHelp("?", "keys")),
	quit:          key.NewBinding(key.WithKeys("q"), key.WithHelp("q", "quit")),
	abort:         key.NewBinding(key.WithKeys("ctrl+c"), key.WithHelp("ctrl+c", "quit")),
}

func reverse(msg tea.KeyPressMsg) bool {
	switch msg.String() {
	case "shift+tab", "left", "up", "k":
		return true
	}
	return false
}

func (m model) ShortHelp() []key.Binding {
	switch {
	case m.popup == popupDetails:
		return []key.Binding{keys.scroll, keys.close, keys.quit}
	case m.popup == popupServers:
		return []key.Binding{keys.rows, keys.toggleServer, keys.apply, keys.discard, keys.quit}
	case m.edit != nil:
		return []key.Binding{keys.cursor, keys.apply, keys.discard, keys.abort}
	case m.stopPrompt:
		return []key.Binding{keys.confirmStop, keys.quit}
	case m.running() || m.finished():
		bindings := []key.Binding{keys.stop}
		if m.finished() {
			bindings = []key.Binding{keys.setup, keys.runAgain}
		}
		bindings = append(bindings, keys.details)
		if m.multipleRunServers() {
			bindings = append(bindings, keys.latencyServer)
		}
		return append(bindings, keys.help, keys.quit)
	case m.auth != nil:
		return []key.Binding{keys.openSignIn, keys.cancelSignIn, keys.quit}
	}
	return []key.Binding{keys.sections, keys.rows, keys.change, keys.start, keys.help, keys.quit}
}

func (m model) FullHelp() [][]key.Binding {
	all := m.ShortHelp()
	if m.run == nil && m.auth == nil && m.edit == nil && m.popup == popupNone {
		all = append(all[:len(all)-2], keys.recheck)
		if m.canChooseServers() {
			all = append(all, keys.servers)
		}
		if m.canUseAvailable() {
			all = append(all, keys.available)
		}
		all = append(all, keys.automatic, keys.page, keys.help, keys.quit)
	}
	cols := make([][]key.Binding, 0, (len(all)+2)/3)
	for i := 0; i < len(all); i += 3 {
		cols = append(cols, all[i:min(i+3, len(all))])
	}
	return cols
}
