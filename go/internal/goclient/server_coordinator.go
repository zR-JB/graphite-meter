package goclient

import (
	"context"
	"errors"
	"fmt"
	"net/http"
	"slices"
	"sync"
	"time"

	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

// ServerRunSummary is one selected server's part of a run.
type ServerRunSummary struct {
	Server                     wire.ServerEntry
	Throughput                 wire.ThroughputTarget
	LatencyTarget              *wire.LatencyTarget
	Results                    []Result // Latency populations and this server's component of each transfer result.
	TotalDownload, TotalUpload uint64
}

// RunDetails is the run's membership and per-server evidence, in selection order.
type RunDetails struct {
	Servers          []ServerRunSummary
	Participants     []string // Servers still measuring throughput.
	LatencyFocus     string
	Intervals        []AggregationInterval
	OmittedIntervals int
	Failures         []ServerFailure
	Outcome          Outcome
}

// Stage resources: one per active transfer direction, plus the latency population.
const roleLatency = "latency"

type participant struct {
	prepared  PreparedServer
	transport *runner
	removed   bool
	results   []Result
}

func (p *participant) id() string { return p.prepared.Server.ID }

type stageServer struct {
	*participant
	cancelTransfer, cancelLatency context.CancelCauseFunc
	latencyFailed                 bool
}
type readyResource struct{ serverID, role string }
type resourceOutcome struct {
	server *stageServer
	role   string
	result Result
	err    error
	at     time.Time
}
type sampledBoundary struct {
	boundary measurementBoundary
	epoch    int
	final    bool
}

type coordinator struct {
	cfg         Config
	prepared    *PreparedRun
	servers     []*participant
	aggregate   aggregateMeasurements
	failures    []ServerFailure
	started     time.Time
	hasMeasured bool
	emit        func(Event)
}

var errNoSurvivors = errors.New("all selected servers failed")

// runSelection has one stage schedule. Its participants own connections and credentials, never independent
// runs. Server state is released within teardown, which the controller ends when delivery is abandoned.
func runSelection(ctx, teardown context.Context, cfg Config, prepared *PreparedRun, emit func(Event)) (err error) {
	c := &coordinator{cfg: cfg.normalized(), prepared: prepared, started: time.Now(), emit: emit}
	defer func() {
		emit(Event{Kind: EventDone, At: time.Now(), Err: err, Servers: c.details(c.outcome(ctx, err))})
	}()
	if prepared == nil || !prepared.Ready() {
		return errors.New("resolve every selected server before starting")
	}
	streams, err := planRunStreams(c.cfg, prepared.Servers)
	if err != nil {
		return err
	}
	for _, server := range prepared.Servers {
		// Paths and credentials are the server's; the schedule is the run's.
		own := server.config
		own.Warmup = c.cfg.Warmup
		connection := server.Connection
		hc, closeHTTP := protocolClient(own, connection.ThroughputTarget.Protocol, func() *http.Transport { return baseTransport(own) })
		ws, closeWS := websocketClient(own)
		defer closeHTTP()
		defer closeWS()
		r := &runner{cfg: own, streams: streams[server.Server.ID], http: hc, websocketHTTP: ws, target: new(connection.ThroughputTarget), latencyTarget: connection.LatencyTarget, coordinated: &participantCounters{}, idleRTT: connection.PreflightRTT, teardown: teardown}
		r.emit = func(e Event) { e.ServerID = server.Server.ID; emit(e) }
		c.servers = append(c.servers, &participant{prepared: server, transport: r})
	}
	return c.run(ctx)
}

func (c *coordinator) active() []*participant {
	return slices.DeleteFunc(slices.Clone(c.servers), func(s *participant) bool { return s.removed })
}

func (c *coordinator) ids() []string {
	ids := []string{}
	for _, server := range c.active() {
		ids = append(ids, server.id())
	}
	return ids
}

func (c *coordinator) details(outcome Outcome) *RunDetails {
	details := &RunDetails{Participants: c.ids(), Intervals: slices.Clone(c.aggregate.intervals), OmittedIntervals: c.aggregate.omitted, Failures: slices.Clone(c.failures), Outcome: outcome}
	if c.prepared != nil {
		details.LatencyFocus = c.prepared.LatencyFocus
	}
	for _, server := range c.servers {
		total := c.aggregate.totals[server.id()]
		connection := server.prepared.Connection
		details.Servers = append(details.Servers, ServerRunSummary{Server: server.prepared.Server, Throughput: connection.ThroughputTarget, LatencyTarget: connection.LatencyTarget, Results: slices.Clone(server.results), TotalDownload: total.down, TotalUpload: total.up})
	}
	return details
}

// publish shares membership and per-server results; it runs on membership changes and stage ends, not per sample.
func (c *coordinator) publish() {
	c.emit(Event{Kind: EventServers, At: time.Now(), Servers: c.details(OutcomeRunning)})
}

func (c *coordinator) run(ctx context.Context) error {
	c.publish()
	for _, stage := range c.cfg.Plan() {
		if err := c.stage(ctx, stage); err != nil {
			return err
		}
		c.emit(Event{Kind: EventStage, At: time.Now(), Stage: stage.Name, Phase: PhaseFinished})
		c.publish()
	}
	return nil
}

func (c *coordinator) outcome(ctx context.Context, err error) Outcome {
	switch {
	case err == nil && len(c.failures) == 0:
		return OutcomeComplete
	case err == nil:
		return OutcomePartial
	case errors.Is(err, context.Canceled) && ctx.Err() != nil:
		return OutcomeStopped
	case c.hasMeasured:
		return OutcomeIncomplete
	}
	return OutcomeFailed
}

// failure removes a server from the run, or only its latency population when that alone failed.
func (c *coordinator) failure(server *stageServer, stage StagePlan, role string, err error, at time.Time) {
	scope := "throughput"
	if role == roleLatency {
		scope = "latency"
		if server.latencyFailed || server.removed {
			return
		}
		server.latencyFailed = true
		server.cancelLatency(err)
	} else {
		if server.removed {
			return
		}
		server.removed = true
		server.cancelTransfer(err)
		server.cancelLatency(err)
	}
	failure := ServerFailure{ServerID: server.id(), Stage: stage.Name, Scope: scope, Reason: "connection-lost", Message: err.Error(), At: at.Sub(c.started)}
	if _, ok := errors.AsType[*AuthRequiredError](err); ok {
		failure.Reason = "authentication-required"
	}
	c.failures = append(c.failures, failure)
	c.emit(Event{Kind: EventServerFailure, At: at, Stage: stage.Name, ServerID: failure.ServerID, Failure: new(failure)})
	c.publish()
}

// retainLatency keeps one final population per server and stage, even when cleanup follows a failure.
func (c *coordinator) retainLatency(outcome resourceOutcome, normalEnd bool) {
	if outcome.role != roleLatency {
		return
	}
	result := outcome.result
	if normalEnd && errors.Is(outcome.err, context.Canceled) {
		result.Err = nil
	}
	p := outcome.server.participant
	if i := slices.IndexFunc(p.results, func(old Result) bool { return old.Stage == result.Stage && old.Direction == "" }); i >= 0 {
		p.results[i] = result
		return
	}
	p.results = append(p.results, result)
	if result.Stage == StageLatency && result.Latency.P50 > 0 {
		p.transport.idleRTT = result.Latency.P50
	}
}

func (s *stageServer) measure(ctx context.Context, stage StagePlan, role string, gate *stageGate) resourceOutcome {
	outcome := resourceOutcome{server: s, role: role}
	if role == roleLatency {
		stats, err := s.transport.measureLatency(ctx, stage.Name, len(stage.Directions) > 0, stage.Duration, gate)
		outcome.result = Result{Stage: stage.Name, Latency: stats, Elapsed: stats.Elapsed, Err: err}
		outcome.err = err
	} else {
		outcome.err = s.transport.measureDirection(ctx, Direction(role), gate)
	}
	outcome.at = time.Now()
	return outcome
}

type stagePhase int

const (
	phasePrepare stagePhase = iota
	phaseWarmup
	phaseMeasure
)

// stage runs one schedule over an expected-resource set: every resource prepares within stageReadyTimeout,
// the run warms up, then one measured window starts for every population at once. A server that fails after
// the first measured window leaves the run; before it, the run cannot start.
func (c *coordinator) stage(ctx context.Context, stage StagePlan) (stageErr error) {
	stageCtx, cancel := context.WithCancelCause(ctx)
	transfer := len(stage.Directions) > 0
	var roles []string
	for _, dir := range stage.Directions {
		roles = append(roles, string(dir))
	}
	if !transfer || c.cfg.LoadedLatency {
		roles = append(roles, roleLatency)
	}
	// Every resource reports readiness and its outcome at most once, so neither send can block.
	ready := make(chan readyResource, len(c.active())*len(roles))
	outcomes := make(chan resourceOutcome, cap(ready))
	start := make(chan struct{})
	var work sync.WaitGroup
	var servers []*stageServer
	var gates []*stageGate
	c.emit(Event{Kind: EventStage, At: time.Now(), Stage: stage.Name, Phase: PhasePreparing})
	for _, p := range c.active() {
		s := &stageServer{participant: p}
		transferCtx, cancelTransfer := context.WithCancelCause(stageCtx)
		latencyCtx, cancelLatency := context.WithCancelCause(stageCtx)
		s.cancelTransfer, s.cancelLatency = cancelTransfer, cancelLatency
		servers = append(servers, s)
		p.transport.coordinated = &participantCounters{}
		for _, role := range roles {
			own, ownCancel := transferCtx, cancelTransfer
			if role == roleLatency {
				own, ownCancel = latencyCtx, cancelLatency
			}
			gate := &stageGate{cancel: ownCancel, start: start, reportReady: func() { ready <- readyResource{p.id(), role} }}
			gates = append(gates, gate)
			work.Go(func() { outcomes <- s.measure(own, stage, role, gate) })
		}
	}
	measuring, normalEnd := false, false
	sampling := &sampler{c: c, stage: stage, ctx: stageCtx, results: make(chan sampledBoundary, 1)}
	defer func() {
		sampling.stop()
		cancel(stageErr)
		work.Wait()
		close(outcomes)
		for outcome := range outcomes {
			c.retainLatency(outcome, normalEnd)
		}
		if transfer && measuring {
			c.finishTransferStage(stage, stageErr)
		}
	}()

	seen := map[readyResource]bool{}
	missing := func(s *stageServer) []string {
		var out []string
		for _, role := range roles {
			if !s.removed && !(role == roleLatency && s.latencyFailed) && !seen[readyResource{s.id(), role}] {
				out = append(out, role)
			}
		}
		return out
	}
	handle := func(outcome resourceOutcome) error {
		c.retainLatency(outcome, false)
		if outcome.err == nil || ctx.Err() != nil || outcome.server.removed || outcome.role == roleLatency && outcome.server.latencyFailed {
			return nil
		}
		if !c.hasMeasured {
			return fmt.Errorf("%s: %w; resolve the selection before starting", outcome.server.prepared.Server.Name, outcome.err)
		}
		c.failure(outcome.server, stage, outcome.role, outcome.err, outcome.at)
		if len(c.ids()) == 0 {
			if measuring && transfer {
				c.aggregate.begin(stage.Name, nil, time.Since(c.started), "dropout")
			}
			return fmt.Errorf("%w: %w", errNoSurvivors, outcome.err)
		}
		return nil
	}

	phase := phasePrepare
	timer := time.NewTimer(stageReadyTimeout)
	defer timer.Stop()
	var tick <-chan time.Time
	for {
		if phase == phasePrepare && !slices.ContainsFunc(servers, func(s *stageServer) bool { return len(missing(s)) > 0 }) {
			phase = phaseWarmup
			warmup := c.cfg.Warmup
			for _, s := range servers {
				if !s.removed {
					warmup = max(warmup, adaptiveWarmup(c.cfg.Warmup, s.transport.idleRTT))
				}
			}
			if warmup > 0 {
				c.emit(Event{Kind: EventStage, At: time.Now(), Stage: stage.Name, Phase: PhaseWarmup})
			}
			timer.Reset(warmup)
		}
		select {
		case <-ctx.Done():
			return context.Cause(ctx)
		case resource := <-ready:
			seen[resource] = true
		case outcome := <-outcomes:
			before := len(c.ids())
			if err := handle(outcome); err != nil {
				return err
			}
			if phase == phaseMeasure && transfer && len(c.ids()) != before {
				sampling.reset()
			}
		case <-tick:
			if !sampling.ending {
				sampling.capture(false)
			}
		case sample := <-sampling.results:
			done, err := sampling.observe(sample, servers)
			if done {
				normalEnd = err == nil
				return err
			}
		case now := <-timer.C:
			switch phase {
			case phasePrepare:
				failure := fmt.Errorf("server resources were not ready within %v", stageReadyTimeout)
				if !c.hasMeasured {
					return failure
				}
				for _, s := range servers {
					for _, role := range missing(s) {
						c.failure(s, stage, role, failure, now)
					}
				}
				if len(c.ids()) == 0 {
					return errNoSurvivors
				}
			case phaseWarmup:
				started, initial, err := c.openWindow(stageCtx, stage, servers, outcomes, handle)
				if err != nil {
					return err
				}
				for _, gate := range gates {
					gate.boundaryStart = started
				}
				phase, measuring, c.hasMeasured = phaseMeasure, true, true
				c.emit(Event{Kind: EventStage, At: started, Stage: stage.Name, Phase: PhaseMeasuring})
				close(start)
				timer.Reset(time.Until(started.Add(stage.Duration)))
				if transfer {
					ticker := time.NewTicker(250 * time.Millisecond)
					defer ticker.Stop()
					tick = ticker.C
					sampling.begin(started, initial)
				}
			case phaseMeasure:
				if !transfer {
					normalEnd = true
					return nil
				}
				sampling.ending = true
				sampling.capture(true)
			}
		}
	}
}

// openWindow captures the baseline for every population, then starts the measured window. Receiver
// checkpoints finish preparation; the client populations begin only once they reply.
func (c *coordinator) openWindow(ctx context.Context, stage StagePlan, servers []*stageServer, outcomes <-chan resourceOutcome, handle func(resourceOutcome) error) (time.Time, measurementBoundary, error) {
	initial := c.capture(ctx, stage, c.active())
	if stage.Name == StageUpload || stage.Name == StageBidirectional {
		for _, s := range servers {
			if !s.removed && initial.up[s.id()] == nil {
				failure := errors.New("receiver checkpoint unavailable before measurement")
				if !c.hasMeasured {
					return time.Time{}, initial, fmt.Errorf("%s: %w", s.prepared.Server.Name, failure)
				}
				c.failure(s, stage, string(Up), failure, time.Now())
			}
		}
		if len(c.ids()) == 0 {
			return time.Time{}, initial, errNoSurvivors
		}
	}
	// A failure reported while the baselines were taken belongs before the window.
	for drained := false; !drained; {
		select {
		case outcome := <-outcomes:
			if err := handle(outcome); err != nil {
				return time.Time{}, initial, err
			}
		default:
			drained = true
		}
	}
	started := time.Now()
	initial.at = started.Sub(c.started)
	for _, p := range c.active() {
		initial.down[p.id()] = p.transport.coordinated.download()
	}
	if len(stage.Directions) > 0 {
		c.aggregate.begin(stage.Name, c.ids(), initial.at, "stage-start")
		c.aggregate.observe(initial)
	}
	return started, initial, nil
}

// sampler owns a transfer stage's periodic boundaries. One capture is in flight at a time; a membership
// change starts a new epoch, so a boundary taken across it is discarded and retaken.
type sampler struct {
	c            *coordinator
	stage        StagePlan
	ctx          context.Context
	results      chan sampledBoundary
	epoch        int
	inFlight     bool
	ending       bool
	cancel       context.CancelFunc
	work         sync.WaitGroup
	lastBytes    map[string]byteLedger
	lastMovement map[string]map[Direction]time.Time
}

func (s *sampler) begin(started time.Time, initial measurementBoundary) {
	s.lastBytes, s.lastMovement = map[string]byteLedger{}, map[string]map[Direction]time.Time{}
	for _, p := range s.c.active() {
		bytes := byteLedger{down: initial.down[p.id()]}
		if snapshot := initial.up[p.id()]; snapshot != nil {
			bytes.up = snapshot.Bytes
		}
		s.lastBytes[p.id()] = bytes
		s.lastMovement[p.id()] = map[Direction]time.Time{Down: started, Up: started}
	}
}

func (s *sampler) capture(final bool) {
	if s.inFlight {
		return
	}
	s.inFlight = true
	ctx, cancel := context.WithCancel(s.ctx)
	s.cancel = cancel
	participants, epoch := s.c.active(), s.epoch
	s.work.Go(func() {
		defer cancel()
		s.results <- sampledBoundary{s.c.capture(ctx, s.stage, participants), epoch, final}
	})
}

// reset follows a membership change: the survivors start a new interval and their rate is unknown until it has two boundaries.
func (s *sampler) reset() {
	s.epoch++
	if s.cancel != nil {
		s.cancel()
	}
	s.c.aggregate.begin(s.stage.Name, s.c.ids(), time.Since(s.c.started), "dropout")
	s.c.emitUnavailable(s.stage)
	s.capture(s.ending)
}

func (s *sampler) stop() {
	if s.cancel != nil {
		s.cancel()
	}
	s.work.Wait()
}

// observe folds one boundary into the aggregate and applies the fetch-stream liveness rule.
// It reports done when the stage ends, with the error that ends it.
func (s *sampler) observe(sample sampledBoundary, servers []*stageServer) (bool, error) {
	c := s.c
	s.inFlight = false
	if sample.epoch != s.epoch {
		s.capture(s.ending)
		return false, nil
	}
	if window := c.aggregate.observe(sample.boundary); window != nil {
		c.emitRates(s.stage, *window)
	} else {
		c.emitUnavailable(s.stage)
	}
	removed := false
	for _, server := range servers {
		if server.removed {
			continue
		}
		id := server.id()
		bytes := byteLedger{down: sample.boundary.down[id]}
		if snapshot := sample.boundary.up[id]; snapshot != nil {
			bytes.up = snapshot.Bytes
		} else {
			bytes.up = sample.boundary.observedUp[id].maximum
		}
		for _, dir := range s.stage.Directions {
			if dir == Down && bytes.down > s.lastBytes[id].down || dir == Up && bytes.up > s.lastBytes[id].up {
				s.lastMovement[id][dir] = time.Now()
			} else if !s.ending && server.transport.targetTransport() == wire.TransportFetchStream && time.Since(s.lastMovement[id][dir]) >= busRedialWindow {
				c.failure(server, s.stage, string(dir), fmt.Errorf("%s stopped delivering bytes for %v", dir, busRedialWindow), time.Now())
				removed = true
				break
			}
		}
		s.lastBytes[id] = bytes
	}
	switch {
	case len(c.ids()) == 0:
		c.aggregate.begin(s.stage.Name, nil, time.Since(c.started), "dropout")
		return true, errNoSurvivors
	case removed:
		s.reset()
	case sample.final:
		return true, nil
	case s.ending:
		s.capture(true)
	}
	return false, nil
}

// capture reads every participant's counters, then its receiver checkpoint. A checkpoint that keeps failing
// for the capture deadline leaves that receiver without a boundary.
func (c *coordinator) capture(ctx context.Context, stage StagePlan, servers []*participant) measurementBoundary {
	boundary := measurementBoundary{at: time.Since(c.started), down: map[string]uint64{}, up: map[string]*ReceiverSnapshot{}, observedUp: map[string]uploadLedger{}}
	for _, server := range servers {
		boundary.down[server.id()] = server.transport.coordinated.download()
		if id, bytes, _ := server.transport.coordinated.upload(); id != "" {
			boundary.observedUp[server.id()] = uploadLedger{id, bytes}
		}
	}
	if stage.Name != StageUpload && stage.Name != StageBidirectional {
		return boundary
	}
	ctx, cancel := context.WithTimeout(ctx, 1500*time.Millisecond)
	defer cancel()
	snapshots := make([]*ReceiverSnapshot, len(servers))
	var work sync.WaitGroup
	for i, server := range servers {
		work.Go(func() { snapshots[i], _ = server.transport.receiverCheckpoint(ctx) })
	}
	work.Wait()
	for i, server := range servers {
		boundary.up[server.id()] = snapshots[i]
	}
	return boundary
}

func (c *coordinator) emitRates(stage StagePlan, window AggregateWindow) {
	for _, dir := range stage.Directions {
		rate := window.DownBytesPerSec
		if dir == Up {
			rate = window.UpBytesPerSec
		}
		if rate != nil {
			var total uint64
			for _, bytes := range c.aggregate.stageTotals[stage.Name] {
				total += bytes.of(dir)
			}
			c.emit(Event{Kind: EventThroughput, At: time.Now(), Stage: stage.Name, Direction: dir, Throughput: ThroughputSample{BytesPerSec: *rate, TotalBytes: total}})
		}
	}
}

func (c *coordinator) emitUnavailable(stage StagePlan) {
	for _, dir := range stage.Directions {
		c.emit(Event{Kind: EventThroughput, At: time.Now(), Stage: stage.Name, Direction: dir, Throughput: ThroughputSample{Unavailable: true}})
	}
}

// finishTransferStage publishes the combined result and gives each server its component of the latest window.
func (c *coordinator) finishTransferStage(stage StagePlan, stageErr error) {
	for _, dir := range stage.Directions {
		result := c.aggregate.result(stage.Name, dir)
		result.Err = stageErr
		c.emit(Event{Kind: EventResult, At: time.Now(), Stage: stage.Name, Direction: dir, Result: new(result)})
		for _, server := range c.servers {
			own := Result{Stage: stage.Name, Direction: dir, Unavailable: true}
			total := c.aggregate.stageTotals[stage.Name][server.id()]
			own.TotalBytes = total.of(dir)
			for _, interval := range slices.Backward(c.aggregate.intervals) {
				if interval.Stage != stage.Name || interval.Window == nil {
					continue
				}
				components := interval.Window.Down
				if dir == Up {
					components = interval.Window.Up
				}
				if i := slices.IndexFunc(components, func(w ComponentWindow) bool { return w.ServerID == server.id() }); i >= 0 {
					own.MeanBps, own.Elapsed = components[i].BytesPerSec, components[i].Duration
					own.Unavailable = components[i].Duration < minimumSurvivorEvidence
					break
				}
			}
			if server.removed {
				own.Err = errors.New("earlier partial measurement")
			}
			server.results = append(server.results, own)
		}
	}
}
