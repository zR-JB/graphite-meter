package main

import (
	"context"
	"slices"
	"strings"
	"time"

	"charm.land/bubbles/v2/key"
	"charm.land/bubbles/v2/spinner"
	tea "charm.land/bubbletea/v2"
	"github.com/charmbracelet/colorprofile"
	"github.com/zR-JB/graphite-meter/go/internal/goclient"
)

type (
	preparationMsg struct {
		seq int
		run *goclient.PreparedRun
		err error
	}
	prepareDueMsg    struct{ seq int }
	authChallengeMsg struct {
		seq     int
		pending *goclient.PendingAuthorization
		err     error
	}
	authTokenMsg struct {
		seq           int
		token, origin string
		err           error
	}
	eventsMsg struct {
		seq    int
		events []goclient.Event
	}
	freshnessMsg struct{}
	interruptMsg struct{}
)

const (
	// fps is the animation's frame rate, so the needle and readouts glide on fast displays; input paints at once
	// regardless.
	fps           = 120
	frameInterval = time.Second / fps
	// paintFPS paints input within a frame of a 120 Hz display; an unchanged view costs one string compare.
	paintFPS = 120
	// toastFor is how long a notice stays.
	toastFor = 3200 * time.Millisecond
)

type prepareState int

const (
	prepareChecking prepareState = iota
	prepareReady
	prepareSignIn
	prepareFailed
	prepareNoServer
)

// overlay is what opens over the console.
type overlay int

const (
	overlayNone overlay = iota
	overlaySettings
	overlayDetails
	overlayHelp
	overlayChooser
	// overlayConnect asks for the server's address, at a first start or after its check could not reach it.
	overlayConnect
)

type signIn struct {
	pending *goclient.PendingAuthorization
	since   time.Time
	opened  bool
}

// toast is a notice in the bottom bar until it lapses.
type toast struct {
	text  line
	until time.Time
}

// frameCache is what the last frame drew that input reads: its pressable regions and a sheet's scroll.
type frameCache struct {
	hits                    []hit
	scroll, maxScroll, rows int
}

type model struct {
	controller *goclient.Controller
	cfg        goclient.Config
	// remembered is where the server is kept for later starts, and recalled the origin kept there; remembering is
	// whether it is kept: it was at start, or the user turned it on.
	remembered, recalled string
	remembering          bool
	width                int
	height               int
	now, since           time.Time
	st                   styles
	pal                  palette
	// tiles is whether the terminal shows 256 colours or more, which tiles and washes need.
	tiles bool
	spin  spinner.Model
	toast *toast

	row         int
	edit        *editState
	overlay     overlay
	scroll      int
	serverDraft []string
	serverRow   int
	openChooser bool
	hover       target
	frame       *frameCache
	// ticking is whether a spinner tick is on its way; Init sends the first.
	ticking bool

	prepareSeq   int
	preparation  *goclient.Preparation
	prepare      prepareState
	prepareErr   string
	preparedRun  *goclient.PreparedRun
	auth         *signIn
	openApproval func(*goclient.PendingAuthorization)

	runSeq      int
	events      <-chan goclient.Event
	run         *runState
	next        *runState
	stopping    bool
	quitting    bool
	interrupted bool
	resetPrompt bool
	last        goclient.Outcome
}

func newModel(cfg goclient.Config) model {
	controller := goclient.NewController(context.Background())
	ticks := spinner.MiniDot
	ticks.FPS = frameInterval
	now := time.Now()
	m := model{
		controller:   controller,
		preparation:  controller.NewPreparation(cfg, nil),
		cfg:          cfg,
		st:           newStyles(true),
		pal:          newPalette(true, nil),
		tiles:        true,
		openApproval: (*goclient.PendingAuthorization).Open,
		prepareSeq:   1,
		spin:         spinner.New(spinner.WithSpinner(ticks)),
		frame:        &frameCache{},
		ticking:      true,
		now:          now,
		since:        now,
	}
	if cfg.BaseURL == "" {
		m = m.askForServer()
	}
	return m
}

// askForServer opens the server's address for typing, with the one set so far.
func (m model) askForServer() model {
	if m.cfg.BaseURL == "" {
		m.prepare = prepareNoServer
	}
	m.overlay = overlayConnect
	m.beginEdit(catalogueRow, m.cfg.BaseURL)
	return m
}

// setRemembering remembers the server for later starts, keeping a ready one at once, or forgets it, deleting the file.
// The file is a few bytes on local disk, changed here in order, so turning it off never loses to an earlier on.
func (m *model) setRemembering(on bool) {
	if on == m.remembering || m.remembered == "" {
		return
	}
	m.remembering = on
	switch {
	case !on:
		m.recalled = ""
		forget(m.remembered)
	case m.prepare == prepareReady && m.cfg.BaseURL != "":
		m.recalled = m.cfg.BaseURL
		remember(m.remembered, m.recalled)
	}
}

// notify shows text in the bottom bar for a while.
func (m model) notify(text line) model {
	m.toast = &toast{text, m.now.Add(toastFor)}
	return m
}

func (m model) Init() tea.Cmd {
	if m.prepare == prepareNoServer {
		return tea.Batch(tea.RequestBackgroundColor, m.spin.Tick)
	}
	return tea.Batch(tea.RequestBackgroundColor, m.prepareAfter(0), m.spin.Tick)
}

func (m model) running() bool { return m.next != nil || m.run != nil && m.run.running() }

// animating is whether frames change without input: a spinner turns, the needle moves, or a notice waits to lapse.
func (m model) animating() bool {
	moving := m.run != nil && m.run.moving(m.now)
	return m.running() || m.prepare == prepareChecking || m.auth != nil || m.toast != nil || moving
}

func (m model) Update(msg tea.Msg) (tea.Model, tea.Cmd) {
	if _, tick := msg.(spinner.TickMsg); tick {
		return m.update(msg)
	}
	m.now = time.Now()
	next, cmd := m.update(msg)
	if n, ok := next.(model); ok && n.animating() && !n.ticking {
		n.ticking = true
		return n, tea.Batch(cmd, n.spin.Tick)
	}
	return next, cmd
}

func (m model) update(msg tea.Msg) (tea.Model, tea.Cmd) {
	switch msg := msg.(type) {
	case tea.BackgroundColorMsg:
		m.st, m.pal = newStyles(msg.IsDark()), newPalette(msg.IsDark(), msg.Color)
	case tea.ColorProfileMsg:
		m.tiles = msg.Profile >= colorprofile.ANSI256
	case tea.WindowSizeMsg:
		m.width, m.height = msg.Width, msg.Height
	case tea.MouseMotionMsg:
		m.hover = m.targetAt(msg.X, msg.Y)
	case tea.MouseClickMsg:
		if msg.Button == tea.MouseLeft {
			return m.click(m.targetAt(msg.X, msg.Y))
		}
	case tea.MouseWheelMsg:
		step := 1
		if msg.Button == tea.MouseWheelUp {
			step = -1
		}
		switch m.overlay {
		case overlaySettings:
			m.moveFocus(step)
		case overlayDetails, overlayHelp, overlayChooser:
			m.scroll = min(max(m.frame.scroll+3*step, 0), m.frame.maxScroll)
		}
	case tea.PasteMsg:
		// An address pasted over the console is the server to test.
		if m.edit == nil && m.overlay == overlayNone && m.auth == nil && !m.running() {
			m = m.askForServer()
			m.edit.input.SetValue("")
		}
		if m.edit != nil {
			return m.updateEdit(msg)
		}
	case tea.KeyPressMsg:
		return m.handleKey(msg)
	case spinner.TickMsg:
		return m.handleTick(msg)
	case prepareDueMsg:
		if msg.seq == m.prepareSeq {
			return m, prepareRun(m.preparation, m.prepareSeq)
		}
	case preparationMsg:
		return m.handlePreparation(msg)
	case authChallengeMsg:
		return m.handleAuthChallenge(msg)
	case authTokenMsg:
		return m.handleAuthToken(msg)
	case eventsMsg:
		return m.handleEvents(msg)
	case freshnessMsg:
	case interruptMsg:
		return m.interrupt()
	default:
		if m.edit != nil {
			return m.updateEdit(msg)
		}
	}
	return m, nil
}

// targetAt is the topmost pressable region at column x and row y in the last frame.
func (m model) targetAt(x, y int) target {
	for i := len(m.frame.hits) - 1; i >= 0; i-- {
		if h := m.frame.hits[i]; h.area.contains(x, y) {
			return h.target
		}
	}
	return target{}
}

func (m model) click(t target) (tea.Model, tea.Cmd) {
	switch t.kind {
	case targetKey:
		if m.running() {
			return m.stop(), nil
		}
		return m.startRun()
	case targetChip:
		return m.switchStage(t.stage)
	case targetCap:
		if m.edit != nil && m.auth == nil {
			switch t.action {
			case actApply:
				return m.handleEditKey(tea.KeyPressMsg{Code: tea.KeyEnter})
			case actCancel:
				return m.handleEditKey(tea.KeyPressMsg{Code: tea.KeyEscape})
			}
			m.edit = nil
		}
		return m.act(t.action, tea.KeyPressMsg{})
	case targetServer:
		m = m.openSettings(catalogueRow)
	case targetRow:
		return m.clickRow(t.index)
	case targetEntry:
		m.serverRow = t.index
		m.selectEntry()
	case targetLink:
		if m.auth != nil {
			return m.openSignIn()
		}
	case targetRemember:
		m.setRemembering(!m.remembering)
	case targetBackdrop:
		m.overlay, m.edit = overlayNone, nil
	}
	return m, nil
}

func (m model) handleKey(msg tea.KeyPressMsg) (tea.Model, tea.Cmd) {
	switch {
	case key.Matches(msg, keys.abort):
		return m.interrupt()
	case m.auth != nil:
		return m.handleSignInKey(msg)
	case m.edit != nil:
		return m.handleEditKey(msg)
	case key.Matches(msg, keys.quit):
		return m.quit()
	}
	for _, binding := range m.table() {
		if key.Matches(msg, binding.binding) && m.offers(binding.action) {
			return m.act(binding.action, msg)
		}
	}
	return m, nil
}

// act does what action does in the context on top.
func (m model) act(a action, msg tea.KeyPressMsg) (tea.Model, tea.Cmd) {
	switch a {
	case actQuit:
		return m.quit()
	case actHelp:
		if m.overlay == overlayHelp {
			m.overlay = overlayNone
		} else {
			m.overlay, m.scroll = overlayHelp, 0
		}
	case actSettings:
		m = m.openSettings(nil)
	case actDetails:
		m.overlay, m.scroll = overlayDetails, 0
	case actClose:
		// The chooser closes back to the settings, as esc does.
		if m.overlay == overlayChooser {
			m.overlay = overlaySettings
		} else {
			m.overlay = overlayNone
		}
	case actBrowse:
		return m.openSignIn()
	case actAbandon:
		m.invalidatePreparation()
		m.prepare, m.prepareErr = prepareSignIn, ""
		m = m.notify(styled("Sign-in canceled", m.pal.muted))
	case actStart:
		return m.startRun()
	case actStop:
		return m.stop(), nil
	case actStage:
		stages := []goclient.Stage{goclient.StageLatency, goclient.StageDownload, goclient.StageUpload,
			goclient.StageBidirectional}
		if i := strings.IndexRune("1234", rune(msg.Code)); i >= 0 {
			return m.switchStage(stages[i])
		}
	case actLatency:
		m.run.nextFocus()
	case actRecheck:
		m = m.notify(styled("Checking the server again…", m.pal.muted))
		return m.reprepare()
	case actScroll, actPage:
		m.scroll = min(max(m.frame.scroll+scrollStep(msg, m.frame.rows), 0), m.frame.maxScroll)
	default:
		switch m.overlay {
		case overlaySettings:
			return m.handleSettingsKey(a, msg)
		case overlayChooser:
			return m.handleServerChooserKey(a, msg)
		}
	}
	return m, nil
}

// scrollStep is how far a scroll key moves a sheet of rows lines.
func scrollStep(msg tea.KeyPressMsg, rows int) int {
	switch msg.String() {
	case "pgup":
		return -max(rows, 1)
	case "pgdown":
		return max(rows, 1)
	case "home":
		return -1 << 30
	case "end":
		return 1 << 30
	}
	return delta(msg)
}

// switchStage switches stage on or off for the next run.
func (m model) switchStage(stage goclient.Stage) (tea.Model, tea.Cmd) {
	if m.running() {
		return m, nil
	}
	i := slices.IndexFunc(stageRows, func(s *setting) bool { return s.label == stageLabels[stage] })
	if i < 0 {
		return m, nil
	}
	before := m.cfg
	m.setFlag(stageRows[i], !*stageRows[i].flag(&m.cfg))
	return m.recheckIfPathsChanged(before)
}

func (m model) handleSignInKey(msg tea.KeyPressMsg) (tea.Model, tea.Cmd) {
	for _, binding := range signInKeys {
		if key.Matches(msg, binding.binding) {
			return m.act(binding.action, msg)
		}
	}
	return m, nil
}

func (m model) openSignIn() (tea.Model, tea.Cmd) {
	launch, pending := m.openApproval, m.auth.pending
	m.auth.opened = true
	return m, func() tea.Msg {
		launch(pending)
		return nil
	}
}

func delta(msg tea.KeyPressMsg) int {
	if reverse(msg) {
		return -1
	}
	return 1
}

func (m model) handleTick(msg spinner.TickMsg) (tea.Model, tea.Cmd) {
	if m.toast != nil && !msg.Time.Before(m.toast.until) {
		m.toast = nil
	}
	if !m.animating() {
		m.ticking = false
		return m, nil
	}
	var cmd tea.Cmd
	m.spin, cmd = m.spin.Update(msg)
	if cmd == nil {
		return m, nil
	}
	m.now = msg.Time
	return m, cmd
}

// stop stops the run at once.
func (m model) stop() model {
	if m.running() && !m.stopping {
		m.controller.CancelRun()
		m.stopping = true
	}
	return m
}

func (m model) quit() (tea.Model, tea.Cmd) {
	if m.running() {
		m.quitting, m.overlay = true, overlayNone
		m = m.notify(styled("Stopping the test before quitting… ctrl+c quits at once.", m.pal.muted))
		return m.stop(), nil
	}
	return m, tea.Quit
}

func (m model) interrupt() (tea.Model, tea.Cmd) {
	m.interrupted = m.interrupted || m.running()
	if m.quitting || !m.running() {
		return m, tea.Quit
	}
	return m.quit()
}

// spinner is the spinner's glyph now.
func (m model) spinner() string {
	frames := []string{"⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"}
	return frames[int(m.now.Sub(m.since)/(80*time.Millisecond))%len(frames)]
}

// capLine is a key and what it does as a bar's cap: the key on a raised plate, its deed beside it.
func (m model) capLine(key, does string, hovered bool) line {
	plate := m.pal.raised
	if hovered {
		plate = m.pal.selected
	}
	return styled(" "+key+" ", m.pal.text.on(plate)).and(" "+does, m.pal.muted)
}
