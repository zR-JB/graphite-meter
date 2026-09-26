package main

import (
	"context"
	"time"

	"github.com/charmbracelet/bubbles/help"
	"github.com/charmbracelet/bubbles/key"
	"github.com/charmbracelet/bubbles/spinner"
	tea "github.com/charmbracelet/bubbletea"
	"github.com/zR-JB/graphite-meter/go/internal/goclient"
)

// Every reply from background work carries the sequence it was started under; a superseded one is dropped.
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

// prepareState is the setup's readiness, from the most recent path check.
type prepareState int

const (
	prepareChecking prepareState = iota
	prepareReady
	prepareSignIn
	prepareFailed
)

type model struct {
	controller *goclient.Controller
	cfg        goclient.Config
	width      int
	height     int
	now        time.Time
	spin       spinner.Model
	help       help.Model
	notice     string

	// Setup.
	section       int
	row           int
	edit          editState
	latencyChoice string // Selected latency server; empty follows the run's lowest-latency choice.
	serverChooser bool
	serverDraft   []string
	serverRow     int
	openChooser   bool // The chooser opens when the running check delivers a catalogue.

	// Preparation. The controller owns its contexts; prepareSeq discards replies it has superseded.
	prepareSeq   int
	preparation  *goclient.Preparation
	prepare      prepareState
	prepareErr   string
	preparedRun  *goclient.PreparedRun
	auth         *goclient.PendingAuthorization
	authServerID string
	authSince    time.Time
	authOpened   bool
	// openApproval launches the browser. It is a field so a test can watch the call instead of opening a window.
	openApproval func(*goclient.PendingAuthorization)

	// Run. runSeq discards events from a replaced run.
	runSeq        int
	events        <-chan goclient.Event
	run           *runState
	stopPrompt    bool
	detailsOpen   bool
	detailsScroll int
}

const animationFPS = 20

func newModel(cfg goclient.Config) model {
	controller := goclient.NewController(context.Background())
	dial := spinner.MiniDot
	dial.FPS = time.Second / animationFPS
	spin := spinner.New(spinner.WithSpinner(dial))
	spin.Style = accentStyle
	return model{
		controller:   controller,
		preparation:  controller.NewPreparation(cfg),
		cfg:          cfg,
		openApproval: (*goclient.PendingAuthorization).Open,
		prepareSeq:   1,
		spin:         spin,
		help:         newHelp(),
		now:          time.Now(),
	}
}

// Init checks the configured paths at once, as the browser does on load.
func (m model) Init() tea.Cmd {
	return tea.Batch(m.prepareAfter(0), m.spin.Tick)
}

func (m model) running() bool  { return m.run != nil && m.run.outcome == goclient.OutcomeRunning }
func (m model) finished() bool { return m.run != nil && m.run.outcome != goclient.OutcomeRunning }

func (m model) animating() bool {
	return m.running() || m.run == nil && (m.prepare == prepareChecking || m.auth != nil)
}

func (m model) Update(msg tea.Msg) (tea.Model, tea.Cmd) {
	switch msg := msg.(type) {
	case tea.WindowSizeMsg:
		m.width, m.height = msg.Width, msg.Height
	case tea.KeyMsg:
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
		// The clipboard read behind ctrl+v answers with a message only the text input understands.
		if m.edit.row != nil {
			var cmd tea.Cmd
			m.edit.input, cmd = m.edit.input.Update(msg)
			return m, cmd
		}
	}
	return m, nil
}

// handleKey routes by what is on screen; each branch accepts exactly the bindings its footer lists.
func (m model) handleKey(msg tea.KeyMsg) (tea.Model, tea.Cmd) {
	switch {
	case m.detailsOpen:
		return m.handleDetailsKey(msg)
	case m.serverChooser:
		return m.handleServerChooserKey(msg)
	case m.edit.row != nil:
		return m.handleEditKey(msg)
	case key.Matches(msg, keys.quit):
		m.close()
		return m, tea.Quit
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
	}
	return m.handleSetupKey(msg)
}

func (m model) handleRunKey(msg tea.KeyMsg) (tea.Model, tea.Cmd) {
	switch {
	case m.multipleRunServers() && key.Matches(msg, keys.details):
		m.detailsOpen, m.detailsScroll = true, 0
	case m.multipleRunServers() && key.Matches(msg, keys.latencyServer):
		m.run.nextFocus()
	case m.running() && key.Matches(msg, keys.stop):
		m.stopPrompt = true
		m.notice = "Stop the test? esc confirms, any other key continues."
	case m.finished() && key.Matches(msg, keys.setup):
		m.run = nil
		m.notice = ""
	case m.finished() && key.Matches(msg, keys.runAgain):
		return m.startRun()
	}
	return m, nil
}

// handleSignInKey keeps enter on the pending approval: it never reaches a row that would re-prepare and drop it.
func (m model) handleSignInKey(msg tea.KeyMsg) (tea.Model, tea.Cmd) {
	switch {
	case key.Matches(msg, keys.openSignIn):
		m.openApproval(m.auth)
		m.authOpened = true
		m.notice = "Sign-in page opened in the browser."
	case key.Matches(msg, keys.cancelSignIn):
		m.invalidatePreparation()
		m.prepare, m.prepareErr = prepareFailed, "Sign-in canceled."
		m.notice = "Sign-in canceled. Press v to check the paths again."
	case key.Matches(msg, keys.sections), key.Matches(msg, keys.rows):
		m.navigate(msg)
	}
	return m, nil
}

func (m model) handleSetupKey(msg tea.KeyMsg) (tea.Model, tea.Cmd) {
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

func (m *model) navigate(msg tea.KeyMsg) {
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

// handleTick advances the spinner and glides the displayed rates toward the latest samples, once per frame.
func (m model) handleTick(msg spinner.TickMsg) (tea.Model, tea.Cmd) {
	if !m.animating() {
		return m, nil
	}
	m.now = msg.Time
	if m.run != nil {
		for dir, sample := range m.run.rates {
			m.run.displayRates[dir] += (sample.BytesPerSec - m.run.displayRates[dir]) * 0.35
		}
	}
	var cmd tea.Cmd
	m.spin, cmd = m.spin.Update(msg)
	return m, cmd
}

func (m *model) close() {
	m.invalidatePreparation()
	m.controller.Close()
}

// newHelp is the footer renderer, dressed in this program's styles rather than the bubble's defaults.
func newHelp() help.Model {
	h := help.New()
	h.Styles.ShortKey, h.Styles.FullKey = labelStyle, labelStyle
	h.Styles.ShortDesc, h.Styles.FullDesc = mutedStyle, mutedStyle
	h.Styles.ShortSeparator, h.Styles.FullSeparator = subtleRuleStyle, subtleRuleStyle
	h.Styles.Ellipsis = subtleRuleStyle
	return h
}
