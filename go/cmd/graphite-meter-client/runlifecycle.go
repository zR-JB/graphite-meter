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

// This file owns the run lifecycle: the tea.Cmd producers that talk to the
// goclient off the UI goroutine, the handlers for the messages they return,
// and the application of a running measurement's events onto the model.

type stageState int

const (
	stagePending stageState = iota
	stageWarmup
	stageMeasuring
	stageDone
	stageStopped
)

// stageProgress is one row of the run screen's timeline. duration is the
// configured measurement window, which the engine holds to exactly. The engine
// stretches warmup to the measured RTT and never reports that window, so a
// warming stage is timed by elapsed alone.
type stageProgress struct {
	name     string
	duration time.Duration
	state    stageState
	since    time.Time
}

// plannedStages is the enabled stages in the order the engine runs them.
func plannedStages(cfg goclient.Config) []stageProgress {
	var stages []stageProgress
	add := func(on bool, name string, d time.Duration) {
		if on {
			stages = append(stages, stageProgress{name: name, duration: d})
		}
	}
	add(cfg.Stages.Latency, "latency", cfg.LatencyDuration)
	add(cfg.Stages.Download, "download", cfg.DownloadDuration)
	add(cfg.Stages.Upload, "upload", cfg.UploadDuration)
	add(cfg.Stages.Bidirectional, "bidirectional", cfg.BidirectionalDuration)
	return stages
}

func prepareConnection(seq int, cfg goclient.Config) tea.Cmd {
	return func() tea.Msg {
		ctx, cancel := context.WithTimeout(context.Background(), 12*time.Second)
		defer cancel()
		connection, err := goclient.Prepare(ctx, cfg)
		return preparationMsg{seq: seq, connection: connection, err: err}
	}
}

func beginAuthorization(seq int, cfg goclient.Config, authURL string) tea.Cmd {
	return func() tea.Msg {
		p, err := goclient.BeginAuthorization(cfg, authURL)
		return authChallengeMsg{seq: seq, pending: p, err: err}
	}
}

// authWait is how long a browser approval may stay outstanding, and the
// deadline the configure screen counts down against.
const authWait = 2 * time.Minute

func pollAuthorization(seq int, p *goclient.PendingAuthorization) tea.Cmd {
	return func() tea.Msg {
		ctx, cancel := context.WithTimeout(context.Background(), authWait)
		defer cancel()
		token, err := p.Poll(ctx)
		return authTokenMsg{seq: seq, token: token, origin: p.Origin, err: err}
	}
}

// prepareDebounce is the quiet period a configuration change waits out before
// the connection is checked again. Cycling an endpoint row changes the
// configuration on every press, and without the pause each press would tear
// down the checklist and open a connection the next press invalidates.
const prepareDebounce = 350 * time.Millisecond

// reprepare puts the checklist back to the start and schedules a fresh check.
// The last verified connection is kept for the readiness panel to show while
// the new one is unproven, so a burst of keypresses moves the values it edits
// and nothing else.
func (m model) reprepare(cmd tea.Cmd) (tea.Model, tea.Cmd) {
	m.prepareSeq++
	m.prepareStatus = "checking"
	m.prepareStep = stepReach
	m.prepareError = ""
	m.auth = nil
	m.authOpened = false
	tick := tea.Tick(prepareDebounce, func(time.Time) tea.Msg { return prepareDueMsg{seq: m.prepareSeq} })
	return m, tea.Batch(cmd, tick, m.spin.Tick)
}

// handlePrepareDue opens the connection the last configuration change asked
// for. A newer change has already bumped the sequence, so only the final change
// of a burst reaches the network.
func (m model) handlePrepareDue(msg prepareDueMsg) (tea.Model, tea.Cmd) {
	if msg.seq != m.prepareSeq {
		return m, nil
	}
	return m, prepareConnection(m.prepareSeq, m.cfg)
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

func waitDone(seq int, done <-chan error) tea.Cmd {
	return func() tea.Msg {
		return doneMsg{seq: seq, err: <-done}
	}
}

// handleTick advances the spinner and glides the displayed rates toward the
// latest samples, once per animation frame.
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
	var preparationErr *goclient.PreparationError
	preflightDecoded := errors.As(msg.err, &preparationErr)
	if preflightDecoded {
		pf := preparationErr.Preflight
		m.discovery = &pf
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
			return m, tea.Batch(beginAuthorization(m.prepareSeq, m.cfg, authErr.URL), m.spin.Tick)
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
	m.discovery = &pf
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
	return m, tea.Batch(pollAuthorization(msg.seq, msg.pending), m.spin.Tick)
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
		m.apply(event)
	}
	if m.mode == modeRun && m.err == nil {
		return m, waitEvents(m.runSeq, m.events)
	}
	return m, nil
}

func (m model) handleDone(msg doneMsg) (tea.Model, tea.Cmd) {
	if msg.seq != m.runSeq {
		return m, nil
	}
	if authErr, ok := errors.AsType[*goclient.AuthRequiredError](msg.err); ok {
		if m.cancel != nil {
			m.cancel()
		}
		m.complete = true
		m.mode = modeConfigure
		m.prepared = nil
		m.prepareStatus = "authorizing"
		if m.prepareStep < stepPreflight {
			m.prepareStep = stepPreflight
		}
		m.focusServer()
		m.notice = "Authorization expired. Preparing the approval page…"
		return m, tea.Batch(beginAuthorization(m.prepareSeq, m.cfg, authErr.URL), m.spin.Tick)
	}
	if msg.err != nil {
		if strings.Contains(msg.err.Error(), "context canceled") {
			m.status = "canceled"
		} else {
			m.err = msg.err
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

	events := make(chan goclient.Event, 256)
	done := make(chan error, 1)
	ctx, cancel := context.WithCancel(context.Background())
	cfg := m.cfg
	prepared := m.prepared
	go func() {
		run := goclient.Run
		if prepared.FreshFor(cfg) {
			run = func(ctx context.Context, cfg goclient.Config, emit func(goclient.Event)) error {
				return goclient.RunPrepared(ctx, cfg, prepared, emit)
			}
		}
		runErr := run(ctx, cfg, func(e goclient.Event) {
			select {
			case events <- e:
			case <-ctx.Done():
			}
		})
		done <- goclient.ClassifyAuthFailure(ctx, cfg, runErr)
		close(events)
	}()

	m.mode = modeRun
	m.runSeq++
	m.events = events
	m.done = done
	m.cancel = cancel
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
	m.stages = plannedStages(cfg)
	m.notice = "Run started. Press esc to cancel."
	return m, tea.Batch(waitEvents(m.runSeq, events), waitDone(m.runSeq, done), m.spin.Tick)
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
			// The negotiated version is not repeated here: the Throughput row
			// below names the whole committed path, in the words the configure
			// screen used to offer it, and the raw evidence spelling ("h3")
			// beside it was the one place the run screen contradicted itself.
			m.server = fmt.Sprintf("%s %s [%s]", e.Preflight.Server.Name, e.Preflight.Server.Location, e.Message)
			m.status = "connected"
		}
	case goclient.EventStage:
		m.stage = e.Stage
		m.status = e.Message
		m.enterStage(e)
	case goclient.EventThroughput:
		m.rates[e.Direction] = e.Throughput
		if e.Throughput.BytesPerSec > m.peaks[e.Direction] {
			m.peaks[e.Direction] = e.Throughput.BytesPerSec
		}
	case goclient.EventLatency:
		if e.Latency.Lost {
			m.lostStreak++
		} else {
			m.lostStreak = 0
			m.latency = e.Latency
		}
	case goclient.EventResult:
		if e.Result != nil {
			m.results = append(m.results, *e.Result)
			m.finishStage(e.Result.Stage)
		}
	case goclient.EventComplete:
		m.status = "complete"
		m.complete = true
	case goclient.EventError:
		m.err = e.Err
		m.status = "error"
		m.stopStages()
	}
}

// enterStage moves a timeline row into the phase the engine just announced.
// The engine names exactly two, so anything else leaves the row where it is.
func (m *model) enterStage(e goclient.Event) {
	var state stageState
	switch e.Message {
	case "warmup":
		state = stageWarmup
	case "measure":
		state = stageMeasuring
	default:
		return
	}
	for i := range m.stages {
		if m.stages[i].name == e.Stage {
			m.stages[i].state, m.stages[i].since = state, e.At
		}
	}
}

// finishStage closes a timeline row. A stage emits results only once every
// lane stops, so the first result proves the window closed and the rest land
// on a row already done.
func (m *model) finishStage(stage string) {
	for i := range m.stages {
		if m.stages[i].name == stage && m.stages[i].state != stagePending {
			m.stages[i].state = stageDone
		}
	}
}

// stopStages marks whichever stage is mid-flight when a run ends early, so a
// canceled or failed run leaves no row spinning forever.
func (m *model) stopStages() {
	for i := range m.stages {
		if s := m.stages[i].state; s == stageWarmup || s == stageMeasuring {
			m.stages[i].state = stageStopped
		}
	}
}
