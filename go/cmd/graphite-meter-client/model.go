package main

import (
	"context"
	"time"

	"charm.land/bubbles/v2/help"
	"charm.land/bubbles/v2/key"
	"charm.land/bubbles/v2/spinner"
	"charm.land/bubbles/v2/viewport"
	tea "charm.land/bubbletea/v2"
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
)

type prepareState int

const (
	prepareChecking prepareState = iota
	prepareReady
	prepareSignIn
	prepareFailed
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
	width      int
	height     int
	now        time.Time
	st         styles
	spin       spinner.Model
	help       help.Model
	notice     string

	section       int
	row           int
	edit          *editState
	latencyChoice string
	popup         popup
	serverDraft   []string
	serverRow     int
	openChooser   bool
	details       viewport.Model

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
	stopPrompt  bool
	quitting    bool
	interrupted bool
	last        goclient.Outcome
}

func newModel(cfg goclient.Config) model {
	controller := goclient.NewController(context.Background())
	st := newStyles(true)
	dial := spinner.MiniDot
	dial.FPS = time.Second / 30
	h := help.New()
	h.Styles = st.helpStyles()
	return model{
		controller:   controller,
		preparation:  controller.NewPreparation(cfg),
		cfg:          cfg,
		st:           st,
		openApproval: (*goclient.PendingAuthorization).Open,
		prepareSeq:   1,
		spin:         spinner.New(spinner.WithSpinner(dial), spinner.WithStyle(st.accent)),
		help:         h,
		details:      viewport.New(),
		now:          time.Now(),
	}
}

func (m model) Init() tea.Cmd {
	return tea.Batch(tea.RequestBackgroundColor, m.prepareAfter(0), m.spin.Tick)
}

func (m model) running() bool  { return m.run != nil && m.run.outcome == goclient.OutcomeRunning }
func (m model) finished() bool { return m.run != nil && m.run.outcome != goclient.OutcomeRunning }

func (m model) animating() bool {
	return m.running() || m.run == nil && (m.prepare == prepareChecking || m.auth != nil)
}

func (m model) Update(msg tea.Msg) (tea.Model, tea.Cmd) {
	switch msg := msg.(type) {
	case tea.BackgroundColorMsg:
		m.st = newStyles(msg.IsDark())
		m.help.Styles, m.spin.Style = m.st.helpStyles(), m.st.accent
	case tea.WindowSizeMsg:
		m.width, m.height = msg.Width, msg.Height
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
		m.close()
		m.interrupted = true
		return m, tea.Quit
	case m.edit != nil:
		return m.handleEditKey(msg)
	case key.Matches(msg, keys.quit):
		return m.quit()
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
	case key.Matches(msg, keys.help):
		m.help.ShowAll = !m.help.ShowAll
		return m, nil
	case m.run != nil:
		return m.handleRunKey(msg)
	case m.auth != nil:
		return m.handleSignInKey(msg)
	case m.prepare == prepareSignIn && key.Matches(msg, keys.start):
		m.notice = "Sign in first. Press v to request a new code."
		return m, nil
	}
	return m.handleSetupKey(msg)
}

func (m model) handleRunKey(msg tea.KeyPressMsg) (tea.Model, tea.Cmd) {
	switch {
	case key.Matches(msg, keys.details):
		m.popup = popupDetails
		m.details.GotoTop()
	case m.multipleRunServers() && key.Matches(msg, keys.latencyServer):
		m.run.nextFocus()
	case m.running() && key.Matches(msg, keys.stop):
		m.stopPrompt = true
		m.notice = "Stop the test? esc confirms, any other key continues."
	case m.finished() && key.Matches(msg, keys.setup):
		m.run = nil
		m.notice = ""
		return m.reprepare()
	case m.finished() && key.Matches(msg, keys.runAgain):
		return m.startRun()
	}
	return m, nil
}

func (m model) handleSignInKey(msg tea.KeyPressMsg) (tea.Model, tea.Cmd) {
	switch {
	case key.Matches(msg, keys.openSignIn):
		m.openApproval(m.auth.pending)
		m.auth.opened = true
		m.notice = "Sign-in page opened in the browser."
	case key.Matches(msg, keys.cancelSignIn):
		m.invalidatePreparation()
		m.prepare, m.prepareErr = prepareSignIn, ""
		m.notice = "Sign-in canceled. Press v to request a new code."
	}
	return m, nil
}

func (m model) handleSetupKey(msg tea.KeyPressMsg) (tea.Model, tea.Cmd) {
	switch {
	case key.Matches(msg, keys.sections), key.Matches(msg, keys.rows):
		m.navigate(msg)
	case key.Matches(msg, keys.change):
		return m.activate(m.currentRow())
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

func (m *model) navigate(msg tea.KeyPressMsg) {
	step := 1
	if reverse(msg) {
		step = -1
	}
	if key.Matches(msg, keys.sections) {
		m.section = (m.section + step + len(sections)) % len(sections)
	} else {
		m.row += step
	}
	m.row = min(max(m.row, 0), len(sections[m.section].rows)-1)
}

func (m model) handleTick(msg spinner.TickMsg) (tea.Model, tea.Cmd) {
	if !m.animating() {
		return m, nil
	}
	m.now = msg.Time
	if m.run != nil {
		for dir, sample := range m.run.rates {
			m.run.shown[dir] += (sample.BytesPerSec - m.run.shown[dir]) * 0.35
		}
	}
	var cmd tea.Cmd
	m.spin, cmd = m.spin.Update(msg)
	return m, cmd
}

func (m model) quit() (tea.Model, tea.Cmd) {
	if m.running() {
		m.controller.CancelRun()
		m.quitting, m.stopPrompt = true, false
		m.notice = "Stopping the test before quitting…"
		return m, nil
	}
	m.close()
	return m, tea.Quit
}

func (m *model) close() {
	m.invalidatePreparation()
	m.controller.Close()
}
