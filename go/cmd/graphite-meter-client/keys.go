package main

import (
	"charm.land/bubbles/v2/key"
	tea "charm.land/bubbletea/v2"
)

// action is what a key or a cap does.
type action int

const (
	actNone action = iota
	actStart
	actStop
	actSettings
	actDetails
	actHelp
	// actStage switches a stage on or off: 1–4.
	actStage
	actLatency
	actRecheck
	actMove
	actAdjust
	actToggle
	actOpen
	actClose
	actScroll
	actPage
	actSelect
	actApply
	actCancel
	actQuit
)

type keymap struct {
	start, stop, settings, details, help, stages, latency, recheck  key.Binding
	rows, adjust, toggle, open, close, closeSettings, closeDetails  key.Binding
	closeHelp, scroll, page, toggleServer, apply, discard, cursor   key.Binding
	openSignIn, cancelSignIn, startSettings, quit, abort, toggleRow key.Binding
}

var keys = keymap{
	start:         key.NewBinding(key.WithKeys("enter", "space", "r"), key.WithHelp("enter", "start test")),
	stop:          key.NewBinding(key.WithKeys("esc"), key.WithHelp("esc", "stop test")),
	settings:      key.NewBinding(key.WithKeys("s"), key.WithHelp("s", "settings")),
	details:       key.NewBinding(key.WithKeys("d"), key.WithHelp("d", "details")),
	help:          key.NewBinding(key.WithKeys("?"), key.WithHelp("?", "keys")),
	stages:        key.NewBinding(key.WithKeys("1", "2", "3", "4"), key.WithHelp("1–4", "stages on/off")),
	latency:       key.NewBinding(key.WithKeys("l"), key.WithHelp("l", "latency server")),
	recheck:       key.NewBinding(key.WithKeys("v"), key.WithHelp("v", "check the server again")),
	rows:          key.NewBinding(key.WithKeys("up", "down", "k", "j", "tab", "shift+tab"), key.WithHelp("↑↓", "move")),
	adjust:        key.NewBinding(key.WithKeys("left", "right"), key.WithHelp("←→", "change")),
	toggle:        key.NewBinding(key.WithKeys("space"), key.WithHelp("space", "on/off")),
	toggleRow:     key.NewBinding(key.WithKeys("space"), key.WithHelp("space", "on/off")),
	open:          key.NewBinding(key.WithKeys("enter"), key.WithHelp("enter", "edit")),
	close:         key.NewBinding(key.WithKeys("esc"), key.WithHelp("esc", "close")),
	closeSettings: key.NewBinding(key.WithKeys("s")),
	closeDetails:  key.NewBinding(key.WithKeys("d")),
	closeHelp:     key.NewBinding(key.WithKeys("?")),
	startSettings: key.NewBinding(key.WithKeys("r"), key.WithHelp("r", "start test")),
	scroll:        key.NewBinding(key.WithKeys("up", "down", "k", "j"), key.WithHelp("↑↓", "scroll")),
	page:          key.NewBinding(key.WithKeys("pgup", "pgdown", "home", "end"), key.WithHelp("pgdn", "page")),
	toggleServer:  key.NewBinding(key.WithKeys("space"), key.WithHelp("space", "select")),
	apply:         key.NewBinding(key.WithKeys("enter"), key.WithHelp("enter", "apply")),
	discard:       key.NewBinding(key.WithKeys("esc"), key.WithHelp("esc", "cancel")),
	cursor:        key.NewBinding(key.WithKeys("left", "right", "home", "end"), key.WithHelp("←→", "move")),
	openSignIn:    key.NewBinding(key.WithKeys("enter", "o"), key.WithHelp("enter", "open the sign-in page")),
	cancelSignIn:  key.NewBinding(key.WithKeys("esc"), key.WithHelp("esc", "cancel sign-in")),
	quit:          key.NewBinding(key.WithKeys("q"), key.WithHelp("q", "quit")),
	abort:         key.NewBinding(key.WithKeys("ctrl+c"), key.WithHelp("ctrl+c", "quit at once")),
}

// binding is a key and what it does in one context; the bar and the keys sheet read the tables they make up.
type binding struct {
	binding key.Binding
	action  action
}

var (
	consoleKeys = []binding{{keys.start, actStart}, {keys.stop, actStop}, {keys.settings, actSettings},
		{keys.details, actDetails}, {keys.stages, actStage}, {keys.latency, actLatency}, {keys.recheck, actRecheck},
		{keys.help, actHelp}, {keys.quit, actQuit}}
	settingsKeys = []binding{{keys.rows, actMove}, {keys.adjust, actAdjust}, {keys.toggleRow, actToggle},
		{keys.open, actOpen}, {keys.startSettings, actStart}, {keys.close, actClose}, {keys.closeSettings, actClose},
		{keys.help, actHelp}, {keys.quit, actQuit}}
	detailsKeys = []binding{{keys.scroll, actScroll}, {keys.page, actPage}, {keys.close, actClose},
		{keys.closeDetails, actClose}, {keys.help, actHelp}, {keys.quit, actQuit}}
	helpKeys    = []binding{{keys.scroll, actScroll}, {keys.close, actClose}, {keys.closeHelp, actHelp}, {keys.quit, actQuit}}
	chooserKeys = []binding{{keys.rows, actMove}, {keys.toggleServer, actSelect}, {keys.apply, actApply},
		{keys.discard, actCancel}}
	signInKeys = []binding{{keys.openSignIn, actNone}, {keys.cancelSignIn, actNone}, {keys.quit, actQuit}}
	editKeys   = []binding{{keys.cursor, actNone}, {keys.apply, actApply}, {keys.discard, actCancel}}
)

// table is the key table of what is on top.
func (m model) table() []binding {
	switch {
	case m.edit != nil:
		return editKeys
	case m.auth != nil:
		return signInKeys
	}
	switch m.overlay {
	case overlaySettings:
		return settingsKeys
	case overlayDetails:
		return detailsKeys
	case overlayHelp:
		return helpKeys
	case overlayChooser:
		return chooserKeys
	}
	return consoleKeys
}

func (m model) offers(a action) bool {
	switch a {
	case actStop:
		return m.running()
	case actStart, actStage:
		return !m.running()
	case actLatency:
		return m.multipleRunServers()
	case actRecheck:
		return m.cfg.BaseURL != "" && !m.running()
	}
	return true
}

func reverse(msg tea.KeyPressMsg) bool {
	switch msg.String() {
	case "shift+tab", "left", "up", "k":
		return true
	}
	return false
}
