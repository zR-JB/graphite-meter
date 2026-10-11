package main

import (
	"cmp"
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
	if m.cfg.BaseURL == "" {
		m.prepare, m.prepareErr = prepareNoServer, ""
		return m, nil
	}
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
		if m.remembered != "" && m.cfg.BaseURL != m.recalled {
			m.recalled = m.cfg.BaseURL
			expiry = tea.Batch(expiry, remember(m.remembered, m.recalled))
		}
	}
	if m.openChooser && msg.run != nil {
		m.openChooser = false
		next, cmd := m.openServerChooser()
		return next, tea.Batch(cmd, expiry)
	}
	return m, expiry
}

func (m model) challengedServer() string {
	if m.preparedRun == nil {
		return ""
	}
	i := slices.IndexFunc(m.preparedRun.Servers, func(s goclient.PreparedServer) bool { return goclient.IsAuthRequired(s.Err) })
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

type runState struct {
	plan     []goclient.StagePlan
	stages   []stageProgress
	started  time.Time
	finished time.Time
	stage    goclient.Stage
	phase    goclient.Phase
	details  *goclient.RunDetails
	results  []goclient.Result
	rates    map[goclient.Direction]goclient.ThroughputSample
	live     map[goclient.Direction]*liveRate
	sampled  map[goclient.Direction]time.Time
	shown    map[goclient.Direction]glide
	history  map[goclient.Direction]trace
	rtt      map[string]trace
	latest   map[string]goclient.LatencySample
	timeouts map[string]int
	marks    []mark
	focus    string
	pick     string
	outcome  goclient.Outcome
	err      error
	// Scales only grow during a run, so nothing rescales under the eye: the highest combined rate in bytes per second,
	// which sets the strips' top and the dial's scale, and the dial's scale while idle latency is measured.
	peak     float64
	rttScale time.Duration
}

// glide moves a shown value to each new sample over the time between samples, as the browser's readout does.
type glide struct {
	from, to float64
	at       time.Time
	over     time.Duration
}

func (g glide) value(now time.Time) float64 {
	if g.over <= 0 {
		return g.to
	}
	u := min(max(now.Sub(g.at).Seconds()/g.over.Seconds(), 0), 1)
	return g.from + (g.to-g.from)*u
}

// show glides toward v from where the value stands at `at`; a first value snaps.
func (r *runState) show(dir goclient.Direction, v float64, at time.Time, gap time.Duration) {
	g, seen := r.shown[dir]
	if !seen || g.to == 0 {
		r.shown[dir] = glide{to: v}
		return
	}
	r.shown[dir] = glide{from: g.value(at), to: v, at: at, over: min(max(gap, 50*time.Millisecond), 400*time.Millisecond)}
}

type stageState int

const (
	stagePending stageState = iota
	stagePreparing
	stageWarmup
	stageMeasuring
	stageDone
	stagePartial
	stageFailed
	stageStopped
)

type stageProgress struct {
	name     goclient.Stage
	duration time.Duration
	state    stageState
	since    time.Time
}

func newRunState(cfg goclient.Config, started time.Time) *runState {
	r := &runState{
		plan:     cfg.Plan(),
		started:  started,
		rates:    map[goclient.Direction]goclient.ThroughputSample{},
		live:     map[goclient.Direction]*liveRate{},
		sampled:  map[goclient.Direction]time.Time{},
		shown:    map[goclient.Direction]glide{},
		history:  map[goclient.Direction]trace{},
		rtt:      map[string]trace{},
		latest:   map[string]goclient.LatencySample{},
		timeouts: map[string]int{},
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
		frame := time.NewTimer(frameInterval)
		defer frame.Stop()
		for e.Kind != goclient.EventDone {
			select {
			case e, ok = <-events:
				if !ok {
					return eventsMsg{seq: seq, events: batch}
				}
				batch = append(batch, e)
			case <-frame.C:
				return eventsMsg{seq: seq, events: batch}
			}
		}
		return eventsMsg{seq: seq, events: batch}
	}
}

func (m model) startRun() (tea.Model, tea.Cmd) {
	if errors.Is(m.cfg.Validate(), goclient.ErrNoServer) {
		m.run = nil
		return m.askForServer(), nil
	}
	if err := m.cfg.Validate(); err != nil {
		m.notice = blocked + ": " + err.Error() + "."
		m.run, m.row = nil, slices.Index(m.rows(), setupGroups[2].rows[0])
		return m, nil
	}
	m.invalidatePreparation()
	m.runSeq++
	m.now = time.Now()
	m.events = m.controller.Start(m.cfg, m.preparedRun)
	m.next = newRunState(m.cfg, m.now)
	m.stopPrompt, m.popup, m.edit = false, popupNone, nil
	m.notice = "Checking paths before the test. Press esc to stop."
	return m, tea.Batch(waitEvents(m.runSeq, m.events), m.spin.Tick)
}

func (m model) handleEvents(msg eventsMsg) (tea.Model, tea.Cmd) {
	if msg.seq != m.runSeq || !m.running() {
		return m, nil
	}
	for _, event := range msg.events {
		switch {
		case m.next != nil && event.Kind == goclient.EventDone:
			return m.startFailed(event)
		case m.next != nil && event.Kind == goclient.EventServers:
			m.run, m.next, m.notice = m.next, nil, "Test started. Press esc to stop."
			m.body.SetYOffset(0)
		case m.next != nil:
			continue
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
	case goclient.IsAuthRequired(done.Err):
		m.run = nil
		m.notice = "Sign-in required; run graphite-meter-client in a terminal to sign in."
		return m.reprepare()
	case m.last == goclient.OutcomeStopped:
		m.notice = "Test stopped before it started."
	default:
		m.notice = startFailed + ": " + errorText(done.Err)
		return m.reprepare()
	}
	return m, nil
}

func (m model) finishRun(done goclient.Event) (tea.Model, tea.Cmd) {
	m.stopPrompt = false
	r := m.run
	r.adopt(done.Servers)
	r.outcome, r.finished = done.Outcome(), done.At
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
	if goclient.IsAuthRequired(done.Err) {
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
		m.notice = m.serverName(e.ServerID) + ": " + failureLabels[e.Failure.Reason]
	case goclient.EventStage:
		r.stage, r.phase = e.Stage, e.Phase
		if e.Phase == goclient.PhasePreparing {
			clear(r.latest)
			clear(r.timeouts)
			clear(r.rates)
			clear(r.live)
			clear(r.sampled)
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
		case slices.ContainsFunc(r.results, unavailable), e.Stage == goclient.StageLatency && !r.measuredLatency():
			state = stageFailed
		case r.details != nil && slices.ContainsFunc(r.details.Failures, left):
			state = stagePartial
		}
		i := slices.IndexFunc(r.stages, func(s stageProgress) bool { return s.name == e.Stage })
		if i >= 0 && state != stagePending {
			r.stages[i].state, r.stages[i].since = state, e.At
		}
		if e.Phase == goclient.PhaseMeasuring {
			for _, dir := range []goclient.Direction{goclient.Down, goclient.Up} {
				r.sampled[dir] = e.At
			}
			r.marks = append(r.marks, mark{at, e.Stage})
			for dir := range r.history {
				r.history[dir] = r.history[dir].add(at, math.NaN())
			}
			for id := range r.rtt {
				r.rtt[id] = r.rtt[id].add(at, math.NaN())
			}
		}
	case goclient.EventThroughput:
		dir, sample := e.Direction, e.Throughput
		r.rates[dir] = sample
		since, sampled := r.sampled[dir]
		r.sampled[dir] = e.At
		if sample.Unavailable {
			// The window restarts, and so does what it presents.
			delete(r.live, dir)
			r.shown[dir] = glide{}
			r.history[dir] = r.history[dir].add(at, math.NaN())
			break
		}
		if r.live[dir] == nil {
			r.live[dir] = &liveRate{}
		}
		live, gap := r.live[dir], e.At.Sub(since)
		if !sampled || !live.observeRate(sample.BytesPerSec, gap) {
			break
		}
		r.show(dir, live.presented, e.At, gap)
		r.history[dir] = r.history[dir].add(at, live.presented)
		total := 0.0
		for _, l := range r.live {
			total += l.presented
		}
		r.peak = max(r.peak, total)
	case goclient.EventLatency:
		v := math.NaN()
		if e.Latency.TimedOut {
			r.timeouts[e.ServerID]++
		} else {
			r.timeouts[e.ServerID] = 0
			r.latest[e.ServerID] = e.Latency
			if r.stage == goclient.StageLatency && e.ServerID == r.latencyServer() {
				r.rttScale = max(r.rttScale, time.Duration(ceilStep(float64(e.Latency.RTT)*1.25/1e6, 1, 2, 4)*1e6))
			}
			v = float64(e.Latency.RTT)
		}
		r.rtt[e.ServerID] = r.rtt[e.ServerID].add(at, v)
	case goclient.EventResult:
		r.results = append(r.results, *e.Result)
		r.peak = max(r.peak, r.mean(e.Result.Stage))
	}
}

func (r *runState) running() bool { return r.outcome == goclient.OutcomeRunning }

// mean is a stage's measured rate, its directions added.
func (r *runState) mean(stage goclient.Stage) float64 {
	total := 0.0
	for _, result := range r.results {
		if result.Stage == stage && !result.Unavailable {
			total += result.MeanBps
		}
	}
	return total
}

// stripTop is the strips' shared top: the browser's chart step above the highest combined rate.
func (r *runState) stripTop() float64 { return niceCeil(max(r.peak, 1)*8*1.03) / 8 }

// gaugeCeiling is the browser's dial scale for a combined rate: its 1-2-5 step above it with 3% headroom, and from
// a megabit up never under 1 Gbit/s, where most connections fit.
func gaugeCeiling(bytesPerSec float64) float64 {
	bits := bytesPerSec * 8 * 1.03
	top := ceilStep(bits, 1, 2, 5)
	if bits >= 1e6 {
		top = max(top, 1e9)
	}
	return top / 8
}

// window is when `stage` measured: from its mark for its planned duration, or false before it started.
func (r *runState) window(stage goclient.Stage) (float64, float64, bool) {
	i := slices.IndexFunc(r.marks, func(m mark) bool { return m.stage == stage })
	j := slices.IndexFunc(r.plan, func(s goclient.StagePlan) bool { return s.Name == stage })
	if i < 0 || j < 0 {
		return 0, 0, false
	}
	return r.marks[i].t, r.marks[i].t + r.plan[j].Duration.Seconds(), true
}

func (r *runState) measuredLatency() bool {
	if r.details == nil {
		return false
	}
	for _, server := range r.details.Servers {
		if server.Server.ID == r.details.LatencyFocus {
			return slices.ContainsFunc(server.Results, func(result goclient.Result) bool {
				return result.Stage == goclient.StageLatency && result.Direction == "" && result.HasMedian()
			})
		}
	}
	return false
}

func (r *runState) adopt(details *goclient.RunDetails) {
	if details == nil {
		return
	}
	r.details = details
	r.focus = cmp.Or(details.LatencyFocus, r.focus)
	if !slices.Contains(details.Participants, r.pick) {
		r.pick = ""
	}
}

func (r *runState) latencyServer() string { return cmp.Or(r.pick, r.focus) }

func (r *runState) nextFocus() {
	ids := r.details.Participants
	if len(ids) == 0 {
		return
	}
	next := ids[(slices.Index(ids, r.latencyServer())+1)%len(ids)]
	r.pick = next
	if next == r.focus {
		r.pick = ""
	}
}

func (r *runState) meanRates(stage goclient.Stage) string {
	var parts []string
	for _, result := range r.results {
		if result.Stage == stage {
			rate := missing
			if !result.Unavailable {
				rate = fmtRate(result.MeanBps)
			}
			parts = append(parts, arrows[result.Direction]+" "+rate)
		}
	}
	return strings.Join(parts, "  ")
}

func (r *runState) latencyPopulations() map[goclient.Stage]goclient.Result {
	out := map[goclient.Stage]goclient.Result{}
	if r.details == nil {
		return out
	}
	for _, server := range r.details.Servers {
		if server.Server.ID != r.latencyServer() {
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
	switch {
	case m.prepare == prepareNoServer:
		return notStarted
	case m.cfg.Validate() != nil:
		return blocked
	case m.auth != nil && m.auth.opened:
		return checkingSignIn
	case m.prepare == prepareSignIn:
		return pathLabels[pathSignIn]
	case m.prepare == prepareFailed && len(m.readyServers()) == 0:
		return startFailed
	case m.prepare == prepareChecking:
		return "Checking paths"
	}
	return notStarted
}
