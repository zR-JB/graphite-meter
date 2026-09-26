package main

import (
	"context"
	"errors"
	"math"
	"slices"
	"strings"
	"time"

	tea "charm.land/bubbletea/v2"
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
	m.preparation = m.controller.NewPreparation(m.cfg, m.preparedRun)
}

func (m model) reprepare() (tea.Model, tea.Cmd) {
	m.invalidatePreparation()
	m.prepare, m.prepareErr = prepareChecking, ""
	return m, tea.Batch(m.prepareAfter(prepareDebounce), m.spin.Tick)
}

func (m model) handlePreparation(msg preparationMsg) (tea.Model, tea.Cmd) {
	if msg.seq != m.prepareSeq {
		return m, nil
	}
	m.preparedRun = msg.run
	var expiry tea.Cmd
	if msg.run.Ready() {
		expiry = tea.Tick(time.Until(msg.run.VerifiedAt.Add(goclient.PreparationFreshness)), func(time.Time) tea.Msg {
			return freshnessMsg{}
		})
	}
	if msg.run != nil && len(msg.run.Servers) > 0 {
		m.cfg.ServerIDs = msg.run.SelectedIDs()
	}
	if authErr, ok := errors.AsType[*goclient.AuthRequiredError](msg.err); ok {
		m.prepare, m.prepareErr = prepareSignIn, ""
		m.notice = "Sign-in required. Preparing the sign-in page…"
		preparation, seq, origin := m.preparation, m.prepareSeq, m.challengedOrigin()
		return m, func() tea.Msg {
			pending, err := preparation.BeginAuthorization(origin, authErr.URL)
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
		next, cmd := m.openServerChooser()
		return next, tea.Batch(cmd, expiry)
	}
	return m, expiry
}

func isAuthRequired(err error) bool {
	_, ok := errors.AsType[*goclient.AuthRequiredError](err)
	return ok
}

func (m model) challengedServer() string {
	if m.preparedRun == nil {
		return ""
	}
	i := slices.IndexFunc(m.preparedRun.Servers, func(s goclient.PreparedServer) bool { return isAuthRequired(s.Err) })
	if i < 0 {
		return ""
	}
	return m.preparedRun.Servers[i].Server.ID
}

func (m model) challengedOrigin() string {
	if server, ok := m.catalogServer(m.challengedServer()); ok {
		return server.URL
	}
	return m.cfg.BaseURL
}

func (m model) handleAuthChallenge(msg authChallengeMsg) (tea.Model, tea.Cmd) {
	if msg.seq != m.prepareSeq {
		return m, nil
	}
	if msg.err != nil {
		m.prepare, m.prepareErr = prepareFailed, errorText(msg.err)
		return m, nil
	}
	m.auth = &signIn{pending: msg.pending, since: time.Now()}
	m.now = m.auth.since
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
	switch {
	case errors.Is(msg.err, goclient.ErrApprovalExpired):
		m.prepare, m.prepareErr = prepareSignIn, ""
		m.notice = "Sign-in expired. Press v to request a new code."
		return m, nil
	case msg.err != nil:
		m.prepare, m.prepareErr = prepareFailed, errorText(msg.err)
		m.notice = ""
		return m, nil
	}
	if issuer, err := wire.CanonicalOrigin(m.challengedOrigin()); err != nil || !strings.EqualFold(issuer, msg.origin) {
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

const historyPoints = 480

type runState struct {
	plan     []goclient.StagePlan
	stages   []stageProgress
	started  time.Time
	stage    goclient.Stage
	phase    goclient.Phase
	details  *goclient.RunDetails
	results  []goclient.Result
	rates    map[goclient.Direction]goclient.ThroughputSample
	shown    map[goclient.Direction]float64
	history  map[goclient.Direction][]point
	rtt      map[string][]point
	latest   map[string]goclient.LatencySample
	timeouts map[string]int
	marks    []mark
	focus    string
	outcome  goclient.Outcome
	err      error
}

type stageState int

const (
	stagePending stageState = iota
	stagePreparing
	stageWarmup
	stageMeasuring
	stageDone
	stagePartial
	stageIncomplete
	stageStopped
)

type stageProgress struct {
	name     goclient.Stage
	duration time.Duration
	state    stageState
	since    time.Time
}

func newRunState(cfg goclient.Config, focus string, started time.Time) *runState {
	r := &runState{
		plan:     cfg.Plan(),
		started:  started,
		rates:    map[goclient.Direction]goclient.ThroughputSample{},
		shown:    map[goclient.Direction]float64{},
		history:  map[goclient.Direction][]point{},
		rtt:      map[string][]point{},
		latest:   map[string]goclient.LatencySample{},
		timeouts: map[string]int{},
		focus:    focus,
		outcome:  goclient.OutcomeRunning,
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
	if err := m.cfg.Validate(); err != nil {
		m.notice = blocked + ": " + err.Error() + "."
		m.run, m.section, m.row = nil, 1, 0
		return m, nil
	}
	m.invalidatePreparation()
	focus := ""
	if slices.Contains(m.cfg.ServerIDs, m.latencyChoice) {
		focus = m.latencyChoice
	}
	m.runSeq++
	m.now = time.Now()
	m.events = m.controller.Start(m.cfg, m.preparedRun)
	m.next = newRunState(m.cfg, focus, m.now)
	m.stopPrompt, m.popup = false, popupNone
	m.notice = "Checking paths before the test. Press esc to stop."
	return m, tea.Batch(waitEvents(m.runSeq, m.events), m.spin.Tick)
}

func (m model) handleEvents(msg eventsMsg) (tea.Model, tea.Cmd) {
	if msg.seq != m.runSeq || !m.running() {
		return m, nil
	}
	m.now = time.Now()
	for _, event := range msg.events {
		switch {
		case m.next != nil && event.Kind == goclient.EventDone:
			return m.startFailed(event)
		case m.next != nil && event.Kind == goclient.EventServers:
			m.run, m.next, m.notice = m.next, nil, "Test started. Press esc to stop."
			m.body.SetYOffset(0)
		case event.Kind == goclient.EventDone:
			return m.finishRun(event)
		}
		m.apply(event)
	}
	return m, waitEvents(m.runSeq, m.events)
}

func (m model) startFailed(done goclient.Event) (tea.Model, tea.Cmd) {
	m.next, m.stopPrompt, m.last = nil, false, done.Outcome()
	switch {
	case m.quitting:
		return m, tea.Quit
	case isAuthRequired(done.Err):
		m.run = nil
		m.notice = "Sign-in required; run graphite-meter-client in a terminal to sign in."
		return m.reprepare()
	case m.last == goclient.OutcomeStopped:
		m.notice = "Test stopped before it started."
	default:
		m.notice = blocked + ": " + errorText(done.Err)
	}
	return m, nil
}

func (m model) finishRun(done goclient.Event) (tea.Model, tea.Cmd) {
	m.stopPrompt = false
	r := m.run
	r.adopt(done.Servers)
	r.outcome = done.Outcome()
	m.last = r.outcome
	if done.Err != nil && !errors.Is(done.Err, context.Canceled) {
		r.err = done.Err
	}
	for i := range r.stages {
		if s := r.stages[i].state; s == stagePreparing || s == stageWarmup || s == stageMeasuring {
			r.stages[i].state = stageStopped
		}
	}
	m.notice = ""
	if m.quitting {
		return m, tea.Quit
	}
	if isAuthRequired(done.Err) {
		m.run = nil
		m.notice = "Sign-in expired. Checking the selected servers…"
		return m.reprepare()
	}
	return m, nil
}

func (m *model) apply(e goclient.Event) {
	r := m.run
	at := e.At.Sub(r.started).Seconds()
	switch e.Kind {
	case goclient.EventServers:
		r.adopt(e.Servers)
	case goclient.EventServerFailure:
		m.notice = m.serverName(e.ServerID) + ": " + errorText(e.Failure.Err)
	case goclient.EventStage:
		r.stage, r.phase = e.Stage, e.Phase
		if e.Phase == goclient.PhasePreparing {
			clear(r.latest)
			clear(r.timeouts)
			clear(r.rates)
			clear(r.shown)
		}
		state := map[goclient.Phase]stageState{
			goclient.PhasePreparing: stagePreparing,
			goclient.PhaseWarmup:    stageWarmup,
			goclient.PhaseMeasuring: stageMeasuring,
			goclient.PhaseFinished:  stageDone,
		}[e.Phase]
		unavailable := func(result goclient.Result) bool { return result.Stage == e.Stage && result.Unavailable }
		left := func(f goclient.ServerFailure) bool { return f.Stage == e.Stage }
		switch {
		case state != stageDone:
		case slices.ContainsFunc(r.results, unavailable):
			state = stageIncomplete
		case r.details != nil && slices.ContainsFunc(r.details.Failures, left):
			state = stagePartial
		}
		i := slices.IndexFunc(r.stages, func(s stageProgress) bool { return s.name == e.Stage })
		if i >= 0 && state != stagePending {
			r.stages[i].state, r.stages[i].since = state, e.At
		}
		if e.Phase == goclient.PhaseMeasuring {
			r.marks = append(r.marks, mark{at, compactStage(e.Stage)})
			gap := point{at, math.NaN()}
			for dir := range r.history {
				r.history[dir] = appendPoint(r.history[dir], gap)
			}
			for id := range r.rtt {
				r.rtt[id] = appendPoint(r.rtt[id], gap)
			}
		}
	case goclient.EventThroughput:
		r.rates[e.Direction] = e.Throughput
		v := e.Throughput.BytesPerSec
		if e.Throughput.Unavailable {
			r.shown[e.Direction], v = 0, math.NaN()
		}
		r.history[e.Direction] = appendPoint(r.history[e.Direction], point{at, v})
	case goclient.EventLatency:
		v := math.NaN()
		if e.Latency.TimedOut {
			r.timeouts[e.ServerID]++
		} else {
			r.timeouts[e.ServerID] = 0
			r.latest[e.ServerID] = e.Latency
			v = float64(e.Latency.RTT)
		}
		r.rtt[e.ServerID] = appendPoint(r.rtt[e.ServerID], point{at, v})
	case goclient.EventResult:
		r.results = append(r.results, *e.Result)
	}
}

func appendPoint(points []point, p point) []point {
	if len(points) >= historyPoints {
		points = slices.Delete(points, 0, len(points)-historyPoints+1)
	}
	return append(points, p)
}

func (r *runState) live() bool { return r.outcome == goclient.OutcomeRunning }

func (r *runState) adopt(details *goclient.RunDetails) {
	if details == nil {
		return
	}
	r.details = details
	if !slices.ContainsFunc(details.Servers, func(s goclient.ServerRunSummary) bool { return s.Server.ID == r.focus }) {
		r.focus = details.LatencyFocus
	}
}

func (r *runState) nextFocus() {
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
	if m.next != nil {
		return "Checking paths"
	}
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
	if m.prepare == prepareSignIn || m.cfg.Validate() != nil {
		return blocked
	}
	return notStarted
}
