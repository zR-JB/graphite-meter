package main

import (
	"context"
	"image/color"
	"slices"
	"strings"
	"time"

	"charm.land/bubbles/v2/help"
	"charm.land/bubbles/v2/key"
	"charm.land/bubbles/v2/spinner"
	"charm.land/bubbles/v2/viewport"
	tea "charm.land/bubbletea/v2"
	uv "github.com/charmbracelet/ultraviolet"
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
	fps           = 30
	frameInterval = time.Second / fps
	// paintFPS paints input within a frame of a 120 Hz display; an unchanged view costs one string compare.
	paintFPS = 120
)

type prepareState int

const (
	prepareChecking prepareState = iota
	prepareReady
	prepareSignIn
	prepareFailed
	prepareNoServer
)

type popup int

const (
	popupNone popup = iota
	popupServers
	popupDetails
)

type signIn struct {
	pending *goclient.PendingAuthorization
	since   time.Time
	opened  bool
}

type model struct {
	controller *goclient.Controller
	cfg        goclient.Config
	// remembered is where the last server that prepared is kept, recalled the origin kept there.
	remembered, recalled string
	width                int
	height               int
	now                  time.Time
	st                   styles
	spin                 spinner.Model
	help                 help.Model
	notice               string

	row         int
	advanced    bool
	edit        *editState
	popup       popup
	serverDraft []string
	serverRow   int
	openChooser bool
	body        viewport.Model

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
	stopPrompt  bool
	quitting    bool
	interrupted bool
	resetPrompt bool
	last        goclient.Outcome
}

func newModel(cfg goclient.Config) model {
	controller := goclient.NewController(context.Background())
	st := newStyles(true)
	dial := spinner.MiniDot
	dial.FPS = frameInterval
	h := help.New()
	h.Styles, h.ShortSeparator = st.helpStyles(), "   "
	m := model{
		controller:   controller,
		preparation:  controller.NewPreparation(cfg, nil),
		cfg:          cfg,
		st:           st,
		openApproval: (*goclient.PendingAuthorization).Open,
		prepareSeq:   1,
		spin:         spinner.New(spinner.WithSpinner(dial), spinner.WithStyle(st.accent)),
		help:         h,
		body:         viewport.New(),
		now:          time.Now(),
	}
	if cfg.BaseURL == "" {
		m = m.askForServer()
	}
	return m
}

// askForServer opens the server's address for typing: nothing can be prepared or tested without one.
func (m model) askForServer() model {
	m.prepare, m.row = prepareNoServer, slices.Index(m.rows(), catalogueRow)
	m.beginEdit(catalogueRow, m.cfg.BaseURL)
	m.notice = "Enter your Graphite Meter server's address, then press enter."
	return m
}

func (m model) Init() tea.Cmd {
	if m.prepare == prepareNoServer {
		return tea.RequestBackgroundColor
	}
	return tea.Batch(tea.RequestBackgroundColor, m.prepareAfter(0), m.spin.Tick)
}

func (m model) running() bool  { return m.next != nil || m.run != nil && m.run.running() }
func (m model) finished() bool { return m.run != nil && !m.running() }

func (m model) animating() bool {
	return m.running() || m.run == nil && (m.prepare == prepareChecking || m.auth != nil)
}

// onKey reports whether a click lands on a key's plate. It reads the plate's colour off the drawn line, so it follows
// every layout and scroll position.
func (m model) onKey(x, y int) bool {
	lines := strings.Split(m.render(), "\n")
	if y < 0 || y >= len(lines) {
		return false
	}
	w, _ := m.size()
	line := uv.NewScreenBuffer(w, 1)
	uv.NewStyledString(lines[y]).Draw(line, line.Bounds())
	cell := line.CellAt(x, 0)
	if cell == nil {
		return false
	}
	// A plate fills a cell's background; its half-block caps fill the foreground.
	paint := cell.Style.Bg
	if cell.Content == "▄" || cell.Content == "▀" {
		paint = cell.Style.Fg
	}
	if paint == nil {
		return false
	}
	same := func(a, b color.Color) bool {
		ar, ag, ab, _ := a.RGBA()
		br, bg, bb, _ := b.RGBA()
		return ar == br && ag == bg && ab == bb
	}
	return same(paint, m.st.plate.GetBackground()) || same(paint, m.st.plateOff.GetBackground())
}

func (m model) Update(msg tea.Msg) (tea.Model, tea.Cmd) {
	switch msg := msg.(type) {
	case tea.BackgroundColorMsg:
		m.st = newStyles(msg.IsDark())
		m.help.Styles, m.spin.Style = m.st.helpStyles(), m.st.accent
	case tea.WindowSizeMsg:
		m.width, m.height = msg.Width, msg.Height
		m.help.SetWidth(max(msg.Width-2, 1))
	case tea.MouseWheelMsg:
		if m.edit == nil && m.auth == nil && !m.stopPrompt {
			m.body = m.bodyViewport(m.layout())
			m.body, _ = m.body.Update(msg)
		}
	case tea.MouseClickMsg:
		if msg.Button != tea.MouseLeft || m.popup != popupNone || m.edit != nil || m.auth != nil || !m.onKey(msg.X, msg.Y) {
			break
		}
		// A key does what its cap says: Stop while running, otherwise start or run again.
		if m.running() {
			return m.handleKey(tea.KeyPressMsg{Code: tea.KeyEscape})
		}
		return m.handleKey(tea.KeyPressMsg{Code: 'r', Text: "r"})
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

func (m model) handleKey(msg tea.KeyPressMsg) (tea.Model, tea.Cmd) {
	switch {
	case key.Matches(msg, keys.abort):
		return m.interrupt()
	case m.edit != nil:
		return m.handleEditKey(msg)
	case key.Matches(msg, keys.quit):
		return m.quit()
	case key.Matches(msg, keys.help) && !m.stopPrompt:
		m.help.ShowAll = !m.help.ShowAll
		return m, nil
	case m.popup == popupDetails:
		return m.handleDetailsKey(msg)
	case m.popup == popupServers:
		return m.handleServerChooserKey(msg)
	case m.stopPrompt:
		m.stopPrompt = false
		if key.Matches(msg, keys.confirmStop) {
			m.controller.CancelRun()
			m.notice = "Stopping the test…"
		} else {
			m.notice = "Test continues."
		}
		return m, nil
	case key.Matches(msg, keys.page), m.run != nil && key.Matches(msg, keys.scroll):
		m.scrollBody(msg)
		return m, nil
	case m.run != nil || m.next != nil:
		return m.handleRunKey(msg)
	case m.auth != nil:
		return m.handleSignInKey(msg)
	case m.prepare == prepareSignIn && key.Matches(msg, keys.start):
		m.notice = blocked + ": sign in first. Press v to request a new code."
		return m, nil
	}
	return m.handleSetupKey(msg)
}

func (m model) handleRunKey(msg tea.KeyPressMsg) (tea.Model, tea.Cmd) {
	switch {
	case key.Matches(msg, keys.details):
		m.popup = popupDetails
		m.body.SetYOffset(0)
	case m.multipleRunServers() && key.Matches(msg, keys.latencyServer):
		m.run.nextFocus()
	case m.running() && key.Matches(msg, keys.stop):
		m.stopPrompt = true
		m.notice = "Stop the test? esc confirms, any other key continues."
	case m.finished() && key.Matches(msg, keys.setup):
		m.run, m.row = nil, 0
		m.notice = ""
		m.body.SetYOffset(0)
		return m.reprepare()
	case m.finished() && key.Matches(msg, keys.runAgain):
		return m.startRun()
	}
	return m, nil
}

func (m model) handleSignInKey(msg tea.KeyPressMsg) (tea.Model, tea.Cmd) {
	switch {
	case key.Matches(msg, keys.openSignIn):
		launch, pending := m.openApproval, m.auth.pending
		m.auth.opened = true
		m.notice = "Sign-in page opened in the browser."
		return m, func() tea.Msg {
			launch(pending)
			return nil
		}
	case key.Matches(msg, keys.cancelSignIn):
		m.invalidatePreparation()
		m.prepare, m.prepareErr = prepareSignIn, ""
		m.notice = "Sign-in canceled. Press v to request a new code."
	}
	return m, nil
}

func (m model) handleSetupKey(msg tea.KeyPressMsg) (tea.Model, tea.Cmd) {
	if m.resetPrompt && !(key.Matches(msg, keys.change) && m.currentRow() == resetRow) {
		m.resetPrompt = false
		m.notice = "Settings kept."
		return m, nil
	}
	row := m.currentRow()
	switch {
	case key.Matches(msg, keys.rows):
		m.navigate(msg)
	case key.Matches(msg, keys.adjust):
		return m.adjust(row, delta(msg))
	case key.Matches(msg, keys.toggle) && row.flag != nil:
		before := m.cfg
		m.setFlag(row, !*row.flag(&m.cfg))
		return m.recheckIfPathsChanged(before)
	case key.Matches(msg, keys.change):
		return m.activate(row)
	case key.Matches(msg, keys.start):
		return m.startRun()
	case key.Matches(msg, keys.recheck):
		return m.reprepare()
	case key.Matches(msg, keys.servers):
		return m.openServerChooser()
	case key.Matches(msg, keys.available) && m.canUseAvailable():
		return m.useAvailableServers()
	case key.Matches(msg, keys.automatic):
		m.cfg.ThroughputTarget, m.cfg.ThroughputProtocol, m.cfg.ThroughputTransport = "auto", "auto", "auto"
		m.cfg.LatencyTarget, m.cfg.LatencyTransport = "auto", "auto"
		m.notice = "Automatic paths applied to every selected server."
		return m.reprepare()
	}
	return m, nil
}

func delta(msg tea.KeyPressMsg) int {
	if reverse(msg) {
		return -1
	}
	return 1
}

func (m *model) navigate(msg tea.KeyPressMsg) {
	m.row, m.notice = min(max(m.row+delta(msg), 0), len(m.rows())-1), ""
	lw, _, _ := columns(max(m.width, minWidth) - 2)
	_, line := m.setupList(lw - 4)
	m.body = m.bodyViewport(m.layout())
	m.body.EnsureVisible(1+line, 0, 0)
}

func (m model) handleTick(msg spinner.TickMsg) (tea.Model, tea.Cmd) {
	if !m.animating() {
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

func (m model) quit() (tea.Model, tea.Cmd) {
	if m.running() {
		m.controller.CancelRun()
		m.quitting, m.stopPrompt = true, false
		m.notice = "Stopping the test before quitting… ctrl+c quits at once."
		return m, nil
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
