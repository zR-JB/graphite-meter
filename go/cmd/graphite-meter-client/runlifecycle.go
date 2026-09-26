package main

import (
	"context"
	"errors"
	"slices"
	"strings"
	"time"

	tea "github.com/charmbracelet/bubbletea"
	"github.com/zR-JB/graphite-meter/go/internal/goclient"
	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

const prepareDebounce = 350 * time.Millisecond

func prepareRun(preparation *goclient.Preparation, seq int) tea.Cmd {
	return func() tea.Msg {
		run, err := preparation.PrepareRun()
		return preparationMsg{seq: seq, run: run, err: err}
	}
}

func (m model) prepareAfter(delay time.Duration) tea.Cmd {
	seq := m.prepareSeq
	return tea.Tick(delay, func(time.Time) tea.Msg { return prepareDueMsg{seq: seq} })
}

func (m *model) invalidatePreparation() {
	m.prepareSeq++
	m.auth = nil
	m.authOpened = false
}

func (m model) reprepare() (tea.Model, tea.Cmd) {
	m.invalidatePreparation()
	m.preparation = m.controller.NewPreparation(m.cfg)
	m.prepare, m.prepareErr = prepareChecking, ""
	return m, tea.Batch(m.prepareAfter(prepareDebounce), m.spin.Tick)
}

func (m model) handlePreparation(msg preparationMsg) (tea.Model, tea.Cmd) {
	if msg.seq != m.prepareSeq {
		return m, nil
	}
	m.preparedRun = msg.run
	if msg.run != nil && len(msg.run.Servers) > 0 {
		m.cfg.ServerIDs = msg.run.SelectedIDs()
	}
	if authErr, ok := errors.AsType[*goclient.AuthRequiredError](msg.err); ok {
		m.authServerID = ""
		if msg.run != nil {
			challenged := func(s goclient.PreparedServer) bool { return isAuthRequired(s.Err) }
			if i := slices.IndexFunc(msg.run.Servers, challenged); i >= 0 {
				m.authServerID = msg.run.Servers[i].Server.ID
			}
		}
		m.prepare, m.prepareErr = prepareSignIn, ""
		m.notice = "Sign-in required. Preparing the sign-in page…"
		preparation, seq, serverID := m.preparation, m.prepareSeq, m.authServerID
		return m, func() tea.Msg {
			pending, err := preparation.BeginAuthorization(serverID, authErr.URL)
			return authChallengeMsg{seq: seq, pending: pending, err: err}
		}
	}
	switch {
	case msg.err != nil && (msg.run == nil || len(msg.run.Servers) == 0):
		m.prepare, m.prepareErr = prepareFailed, errorText(msg.err)
	case msg.err != nil:
		m.prepare, m.prepareErr = prepareFailed, ""
		if !slices.ContainsFunc(msg.run.Servers, func(s goclient.PreparedServer) bool { return s.Err != nil }) {
			m.prepareErr = errorText(msg.err)
		}
	default:
		m.prepare, m.prepareErr = prepareReady, ""
	}
	if m.openChooser && msg.run != nil {
		m.openChooser = false
		return m.openServerChooser()
	}
	return m, nil
}

func isAuthRequired(err error) bool {
	_, ok := errors.AsType[*goclient.AuthRequiredError](err)
	return ok
}

func (m model) handleAuthChallenge(msg authChallengeMsg) (tea.Model, tea.Cmd) {
	if msg.seq != m.prepareSeq {
		return m, nil
	}
	if msg.err != nil {
		m.prepare, m.prepareErr = prepareFailed, errorText(msg.err)
		return m, nil
	}
	m.auth, m.authOpened = msg.pending, false
	m.authSince, m.now = time.Now(), time.Now()
	m.notice = "Check the code, then press enter to open the sign-in page."
	preparation, pending, seq := m.preparation, msg.pending, msg.seq
	return m, tea.Batch(m.spin.Tick, func() tea.Msg {
		token, err := preparation.PollAuthorization(pending)
		return authTokenMsg{seq: seq, token: token, origin: pending.Origin, err: err}
	})
}

func (m model) handleAuthToken(msg authTokenMsg) (tea.Model, tea.Cmd) {
	if msg.seq != m.prepareSeq {
		return m, nil
	}
	m.auth = nil
	if msg.err != nil {
		m.prepare, m.prepareErr = prepareFailed, errorText(msg.err)
		return m, nil
	}
	expected := m.cfg.BaseURL
	if server, ok := m.catalogServer(m.authServerID); ok {
		expected = server.URL
	}
	if issuer, err := wire.CanonicalOrigin(expected); err != nil || !strings.EqualFold(issuer, msg.origin) {
		m.notice = "The server changed while sign-in was pending, so the approval was discarded."
		return m.reprepare()
	}
	if err := m.controller.AcceptAuthorization(msg.origin, msg.token); err != nil {
		m.prepare, m.prepareErr = prepareFailed, errorText(err)
		return m, nil
	}
	m.notice = "Signed in. Checking the authenticated paths…"
	return m.reprepare()
}

type runState struct {
	plan          []goclient.StagePlan
	stages        []stageProgress
	stage         goclient.Stage
	phase         goclient.Phase
	details       *goclient.RunDetails
	results       []goclient.Result
	rates         map[goclient.Direction]goclient.ThroughputSample
	displayRates  map[goclient.Direction]float64
	peaks         map[goclient.Direction]float64
	latest        map[string]goclient.LatencySample
	timeoutStreak map[string]int
	focus         string
	outcome       goclient.Outcome
	err           error
}

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
	name     goclient.Stage
	duration time.Duration
	state    stageState
	since    time.Time
}

func newRunState(cfg goclient.Config, focus string) *runState {
	r := &runState{
		plan:          cfg.Plan(),
		rates:         map[goclient.Direction]goclient.ThroughputSample{},
		displayRates:  map[goclient.Direction]float64{},
		peaks:         map[goclient.Direction]float64{},
		latest:        map[string]goclient.LatencySample{},
		timeoutStreak: map[string]int{},
		focus:         focus,
		outcome:       goclient.OutcomeRunning,
	}
	for _, stage := range r.plan {
		r.stages = append(r.stages, stageProgress{name: stage.Name, duration: stage.Duration})
	}
	return r
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

func (m model) startRun() (tea.Model, tea.Cmd) {
	if len(m.cfg.Plan()) == 0 {
		m.notice = "Turn on at least one stage before starting."
		m.run, m.section, m.row = nil, 1, 0
		return m, nil
	}
	m.invalidatePreparation()
	focus := ""
	if slices.Contains(m.cfg.ServerIDs, m.latencyChoice) {
		focus = m.latencyChoice
	}
	m.runSeq++
	m.events = m.controller.Start(m.cfg, m.preparedRun)
	m.run = newRunState(m.cfg, focus)
	m.stopPrompt, m.detailsOpen = false, false
	m.now = time.Now()
	m.notice = "Test started. Press esc to stop."
	return m, tea.Batch(waitEvents(m.runSeq, m.events), m.spin.Tick)
}

func (m model) handleEvents(msg eventsMsg) (tea.Model, tea.Cmd) {
	if msg.seq != m.runSeq || m.run == nil {
		return m, nil
	}
	m.now = time.Now()
	for _, event := range msg.events {
		if event.Kind == goclient.EventDone {
			return m.finishRun(event)
		}
		m.apply(event)
	}
	return m, waitEvents(m.runSeq, m.events)
}

func (m model) finishRun(done goclient.Event) (tea.Model, tea.Cmd) {
	m.controller.CancelRun()
	m.stopPrompt = false
	r := m.run
	r.adopt(done.Servers)
	r.outcome = done.Outcome()
	if done.Err != nil && !errors.Is(done.Err, context.Canceled) {
		r.err = done.Err
	}
	for i := range r.stages {
		if s := r.stages[i].state; s == stagePreparing || s == stageWarmup || s == stageMeasuring {
			r.stages[i].state = stageStopped
		}
	}
	m.notice = ""
	if isAuthRequired(done.Err) {
		m.run = nil
		m.notice = "Sign-in expired. Checking the selected servers…"
		return m.reprepare()
	}
	return m, nil
}

func (m *model) apply(e goclient.Event) {
	r := m.run
	switch e.Kind {
	case goclient.EventServers:
		r.adopt(e.Servers)
	case goclient.EventServerFailure:
		m.notice = r.serverName(e.ServerID) + ": " + e.Failure.Message
	case goclient.EventStage:
		r.stage, r.phase = e.Stage, e.Phase
		state := map[goclient.Phase]stageState{
			goclient.PhasePreparing: stagePreparing,
			goclient.PhaseWarmup:    stageWarmup,
			goclient.PhaseMeasuring: stageMeasuring,
			goclient.PhaseFinished:  stageDone,
		}[e.Phase]
		i := slices.IndexFunc(r.stages, func(s stageProgress) bool { return s.name == e.Stage })
		if i >= 0 && state != stagePending {
			r.stages[i].state, r.stages[i].since = state, e.At
		}
	case goclient.EventThroughput:
		r.rates[e.Direction] = e.Throughput
		if e.Throughput.Unavailable {
			r.displayRates[e.Direction] = 0
		}
		r.peaks[e.Direction] = max(r.peaks[e.Direction], e.Throughput.BytesPerSec)
	case goclient.EventLatency:
		if e.Latency.TimedOut {
			r.timeoutStreak[e.ServerID]++
		} else {
			r.timeoutStreak[e.ServerID] = 0
			r.latest[e.ServerID] = e.Latency
		}
	case goclient.EventResult:
		r.results = append(r.results, *e.Result)
	}
}

func (r *runState) adopt(details *goclient.RunDetails) {
	if details == nil {
		return
	}
	r.details = details
	if !slices.ContainsFunc(details.Servers, func(s goclient.ServerRunSummary) bool { return s.Server.ID == r.focus }) {
		r.focus = details.LatencyFocus
	}
}

func (r *runState) serverName(id string) string {
	if r.details != nil {
		for _, s := range r.details.Servers {
			if s.Server.ID == id {
				return s.Server.Name
			}
		}
	}
	return id
}

func (r *runState) nextFocus() {
	if r.details == nil || len(r.details.Servers) < 2 {
		return
	}
	i := slices.IndexFunc(r.details.Servers, func(s goclient.ServerRunSummary) bool { return s.Server.ID == r.focus })
	r.focus = r.details.Servers[(i+1)%len(r.details.Servers)].Server.ID
}

func (r *runState) latencyPopulations() map[goclient.Stage]goclient.Result {
	out := map[goclient.Stage]goclient.Result{}
	if r.details == nil {
		return out
	}
	for _, server := range r.details.Servers {
		if server.Server.ID != r.focus {
			continue
		}
		for _, result := range server.Results {
			if result.Direction == "" {
				out[result.Stage] = result
			}
		}
	}
	return out
}

func (m model) statusLabel() string {
	if r := m.run; r != nil {
		switch {
		case r.outcome != goclient.OutcomeRunning:
			return outcomeLabels[r.outcome]
		case r.stage == "" || r.phase == goclient.PhasePreparing:
			return "Checking paths"
		case r.phase == goclient.PhaseWarmup:
			return "Warmup"
		}
		return stageLabels[r.stage]
	}
	switch m.prepare {
	case prepareChecking:
		return "Checking paths"
	case prepareSignIn:
		return "Sign in"
	case prepareFailed:
		return "Path failed"
	}
	if !m.preparedRun.FreshFor(m.cfg) {
		return "Recheck needed"
	}
	return "Ready"
}

var outcomeLabels = map[goclient.Outcome]string{
	goclient.OutcomeComplete:   "Complete",
	goclient.OutcomePartial:    "Partial",
	goclient.OutcomeIncomplete: "Incomplete",
	goclient.OutcomeStopped:    "Stopped",
	goclient.OutcomeFailed:     "Failed",
}
