package main

import (
	"context"
	"errors"
	"fmt"
	"strings"
	"time"

	"github.com/charmbracelet/bubbles/spinner"
	tea "github.com/charmbracelet/bubbletea"
	"github.com/zR-JB/graphite-meter/go/internal/goclient"
)

type stageState int

const (
	stagePending stageState = iota
	stagePreparing
	stageWarmup
	stageMeasuring
	stageDone
	stageStopped
)

type stageProgress struct {
	name     string
	duration time.Duration
	state    stageState
	since    time.Time
}

// plannedStages is the enabled stages in the order the engine runs them.
func plannedStages(cfg goclient.Config) []stageProgress {
	var stages []stageProgress
	for _, stage := range cfg.Plan() {
		stages = append(stages, stageProgress{name: stage.Name, duration: stage.Duration})
	}
	return stages
}

func prepareConnection(preparation *goclient.Preparation, seq int) tea.Cmd {
	return func() tea.Msg {
		connection, err := preparation.Prepare()
		return preparationMsg{seq: seq, connection: connection, err: err}
	}
}

func beginAuthorization(preparation *goclient.Preparation, seq int, authURL string) tea.Cmd {
	return func() tea.Msg {
		pending, err := preparation.BeginAuthorization(authURL)
		return authChallengeMsg{seq: seq, pending: pending, err: err}
	}
}

func pollAuthorization(preparation *goclient.Preparation, seq int, pending *goclient.PendingAuthorization) tea.Cmd {
	return func() tea.Msg {
		token, err := preparation.PollAuthorization(pending)
		return authTokenMsg{seq: seq, token: token, origin: pending.Origin, err: err}
	}
}

const prepareDebounce = 350 * time.Millisecond

func (m *model) invalidatePreparation() {
	m.prepareSeq++
	m.auth = nil
	m.authOpened = false
}

func (m *model) renewPreparation() {
	m.invalidatePreparation()
	m.preparation = m.controller.NewPreparation(m.cfg)
}

func (m *model) close() {
	m.invalidatePreparation()
	m.controller.Close()
}

func (m model) reprepare(cmd tea.Cmd) (tea.Model, tea.Cmd) {
	m.renewPreparation()
	m.prepareStatus = "checking"
	m.prepareStep = stepReach
	m.prepareError = ""
	tick := tea.Tick(prepareDebounce, func(time.Time) tea.Msg { return prepareDueMsg{seq: m.prepareSeq} })
	return m, tea.Batch(cmd, tick, m.spin.Tick)
}

func (m model) handlePrepareDue(msg prepareDueMsg) (tea.Model, tea.Cmd) {
	if msg.seq != m.prepareSeq {
		return m, nil
	}
	return m, prepareConnection(m.preparation, m.prepareSeq)
}

func waitEvents(seq int, events <-chan goclient.Event) tea.Cmd {
	return func() tea.Msg {
		e, ok := <-events
		if !ok {
			return nil
		}
		batch := []goclient.Event{e}
		for {
			select {
			case e, ok := <-events:
				if !ok {
					return eventsMsg{seq: seq, events: batch}
				}
				batch = append(batch, e)
			default:
				return eventsMsg{seq: seq, events: batch}
			}
		}
	}
}

// handleTick advances the spinner and glides the displayed rates toward the latest samples, once per animation frame.
func (m model) handleTick(msg spinner.TickMsg) (tea.Model, tea.Cmd) {
	if !m.animating() {
		return m, nil
	}
	m.now = msg.Time
	for dir, sample := range m.rates {
		m.displayRates[dir] += (sample.BytesPerSec - m.displayRates[dir]) * 0.35
	}
	var cmd tea.Cmd
	m.spin, cmd = m.spin.Update(msg)
	return m, cmd
}

func (m model) handlePreparation(msg preparationMsg) (tea.Model, tea.Cmd) {
	if msg.seq != m.prepareSeq {
		return m, nil
	}
	preparationErr, preflightDecoded := errors.AsType[*goclient.PreparationError](msg.err)
	if preflightDecoded {
		pf := preparationErr.Preflight
		m.discovery = new(pf)
	}
	if msg.err != nil {
		if authErr, ok := errors.AsType[*goclient.AuthRequiredError](msg.err); ok {
			m.prepareStatus = "authorizing"
			m.prepareStep = stepPreflight
			if preflightDecoded {
				m.prepareStep = stepOrigins
			}
			m.prepareError = ""
			m.focusServer()
			m.notice = "This server requires authorization. Preparing the approval page…"
			return m, tea.Batch(beginAuthorization(m.preparation, m.prepareSeq, authErr.URL), m.spin.Tick)
		}
		m.prepareStatus = "failed"
		m.prepareStep = stepReach
		if preflightDecoded {
			m.prepareStep = stepOrigins
		} else {
			m.discovery = nil
		}
		m.prepareError = msg.err.Error()
		m.prepared = nil
		return m, nil
	}
	m.prepareStatus = "ready"
	m.prepareStep = stepReady
	m.prepareError = ""
	m.prepared = msg.connection
	pf := msg.connection.Preflight
	m.discovery = new(pf)
	return m, nil
}

func (m model) handleAuthChallenge(msg authChallengeMsg) (tea.Model, tea.Cmd) {
	if msg.seq != m.prepareSeq {
		return m, nil
	}
	if msg.err != nil {
		m.prepareStatus = "failed"
		m.prepareError = msg.err.Error()
		return m, nil
	}
	m.auth = msg.pending
	m.authOpened = false
	m.authSince = time.Now()
	m.now = m.authSince
	m.focusServer()
	m.notice = "Check the code below, then press enter to open the approval page."
	return m, tea.Batch(pollAuthorization(m.preparation, msg.seq, msg.pending), m.spin.Tick)
}

func (m model) handleAuthToken(msg authTokenMsg) (tea.Model, tea.Cmd) {
	if msg.seq != m.prepareSeq {
		return m, nil
	}
	m.auth = nil
	if msg.err != nil {
		m.prepareStatus = "failed"
		m.prepareError = msg.err.Error()
		return m, nil
	}
	currentOrigin, err := goclient.CanonicalServerOrigin(m.cfg.BaseURL)
	if err != nil || !strings.EqualFold(currentOrigin, msg.origin) {
		m.notice = "Server changed while approval was pending. Authorization was discarded."
		return m.reprepare(nil)
	}
	m.cfg.AuthToken = msg.token
	m.cfg.AuthOrigin = msg.origin
	m.notice = "Client approved. Verifying authenticated transports…"
	return m.reprepare(nil)
}

func (m model) handleEvents(msg eventsMsg) (tea.Model, tea.Cmd) {
	if msg.seq != m.runSeq {
		return m, nil
	}
	m.now = time.Now()
	for _, event := range msg.events {
		if event.Kind == goclient.EventDone {
			return m.finishRun(event.Err)
		}
		m.apply(event)
	}
	if m.mode == modeRun {
		return m, waitEvents(m.runSeq, m.events)
	}
	return m, nil
}

func (m model) finishRun(err error) (tea.Model, tea.Cmd) {
	m.controller.CancelRun()
	if authErr, ok := errors.AsType[*goclient.AuthRequiredError](err); ok {
		m.renewPreparation()
		m.complete = true
		m.mode = modeConfigure
		m.prepared = nil
		m.prepareStatus = "authorizing"
		m.prepareStep = max(m.prepareStep, stepPreflight)
		m.focusServer()
		m.notice = "Authorization expired. Preparing the approval page…"
		return m, tea.Batch(beginAuthorization(m.preparation, m.prepareSeq, authErr.URL), m.spin.Tick)
	}
	m.status = "complete"
	if err != nil {
		if errors.Is(err, context.Canceled) {
			m.status = "canceled"
		} else {
			m.err = err
			m.status = "error"
		}
	}
	m.stopStages()
	m.complete = true
	m.cancelPrompt = false
	return m, nil
}

func (m model) startRun() (model, tea.Cmd) {
	if !m.cfg.Stages.Latency && !m.cfg.Stages.Download && !m.cfg.Stages.Upload && !m.cfg.Stages.Bidirectional {
		m.notice = "Enable at least one stage before running."
		m.mode = modeConfigure
		m.section = sectionRunSetup
		m.row = 0
		return m, nil
	}
	m.invalidatePreparation()
	events := m.controller.Start(m.cfg, m.prepared)

	m.mode = modeRun
	m.runSeq++
	m.events = events
	m.stage = ""
	m.status = "connecting"
	m.server = ""
	m.target, m.latencyTarget = "", ""
	m.throughputProtocol, m.latencyProtocol = "", ""
	m.throughputTransport, m.latencyTransport = "", ""
	m.err = nil
	m.complete = false
	m.cancelPrompt = false
	m.now = time.Now()
	m.rates = map[goclient.Direction]goclient.ThroughputSample{}
	m.peaks = map[goclient.Direction]float64{}
	m.displayRates = map[goclient.Direction]float64{}
	m.lostStreak = 0
	m.results = nil
	m.latency = goclient.LatencySample{}
	m.stages = plannedStages(m.cfg)
	m.notice = "Run started. Press esc to cancel."
	return m, tea.Batch(waitEvents(m.runSeq, events), m.spin.Tick)
}

func (m *model) apply(e goclient.Event) {
	switch e.Kind {
	case goclient.EventPreflight:
		if e.Preflight != nil {
			m.target = e.ThroughputTarget
			if m.target == "" {
				m.target = e.Message
			}
			m.latencyTarget = e.LatencyTarget
			m.throughputProtocol = e.ThroughputProtocol
			m.throughputTransport = e.ThroughputTransport
			m.latencyTransport = e.LatencyTransport
			m.latencyProtocol = e.LatencyProtocol
			m.server = fmt.Sprintf("%s %s [%s]", e.Preflight.Server.Name, e.Preflight.Server.Location, e.Message)
			m.status = "connected"
		}
	case goclient.EventStage:
		m.stage = e.Stage
		m.status = string(e.Phase)
		m.enterStage(e)
	case goclient.EventThroughput:
		m.rates[e.Direction] = e.Throughput
		m.peaks[e.Direction] = max(m.peaks[e.Direction], e.Throughput.BytesPerSec)
	case goclient.EventLatency:
		if e.Latency.TimedOut {
			m.lostStreak++
		} else {
			m.lostStreak = 0
			m.latency = e.Latency
		}
	case goclient.EventResult:
		if e.Result != nil {
			m.results = append(m.results, *e.Result)
		}
	}
}

func (m *model) enterStage(e goclient.Event) {
	var state stageState
	switch e.Phase {
	case goclient.StagePreparing:
		state = stagePreparing
	case goclient.StageWarmup:
		state = stageWarmup
	case goclient.StageMeasuring:
		state = stageMeasuring
	case goclient.StageFinished:
		state = stageDone
	default:
		return
	}
	for i := range m.stages {
		if m.stages[i].name == e.Stage {
			m.stages[i].state, m.stages[i].since = state, e.At
		}
	}
}

func (m *model) stopStages() {
	for i := range m.stages {
		if s := m.stages[i].state; s == stagePreparing || s == stageWarmup || s == stageMeasuring {
			m.stages[i].state = stageStopped
		}
	}
}
