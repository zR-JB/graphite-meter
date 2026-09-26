package goclient

import (
	"context"
	"errors"
	"fmt"
	"slices"
	"time"

	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

type ServerRunSummary struct {
	Server        wire.ServerEntry
	Throughput    wire.ThroughputTarget
	LatencyTarget *wire.LatencyTarget
	Results       []Result
}

type RunDetails struct {
	Servers          []ServerRunSummary
	Participants     []string
	LatencyFocus     string
	Intervals        []AggregationInterval
	OmittedIntervals int
	Failures         []ServerFailure
	Outcome          Outcome
}

type participant struct {
	prepared  PreparedServer
	transport *runner
	removed   bool
	results   []Result
}

func (p *participant) id() string { return p.prepared.Server.ID }

type coordinator struct {
	cfg         Config
	prepared    *PreparedRun
	servers     []*participant
	aggregate   aggregateMeasurements
	failures    []ServerFailure
	started     time.Time
	hasMeasured bool
	unavailable bool
	emit        func(Event)
}

var (
	errNoSurvivors  = errors.New("all selected servers failed")
	errStageSkipped = errors.New("stage skipped")
)

const (
	sampleInterval        = 250 * time.Millisecond
	checkpointBudget      = 1500 * time.Millisecond
	finalCheckpointBudget = 500 * time.Millisecond
	// A longer gap between sampled boundaries means the client itself stalled.
	maximumBoundaryGap = sampleInterval + checkpointBudget
)

var errHandover = errors.New("stage handed over")

func runSelection(ctx, teardown context.Context, cfg Config, prepared *PreparedRun, emit func(Event)) {
	c := &coordinator{cfg: cfg.normalized(), prepared: prepared, started: time.Now(), emit: emit}
	err := c.start(ctx, teardown)
	emit(Event{Kind: EventDone, At: time.Now(), Err: err, Servers: c.details(c.outcome(ctx, err))})
}

func (c *coordinator) start(ctx, teardown context.Context) error {
	prepared := c.prepared
	if !prepared.runnable() {
		return errors.New("no selected server is ready")
	}
	for _, server := range prepared.Servers {
		if !server.ready() {
			c.servers = append(c.servers, &participant{prepared: server, removed: true})
			failure := ServerFailure{ServerID: server.Server.ID, Scope: ScopeThroughput, Err: server.Err}
			failure.Reason = failureReason(server.Err)
			if plan := c.cfg.Plan(); len(plan) > 0 {
				failure.Stage = plan[0].Name
			}
			c.failures = append(c.failures, failure)
			continue
		}
		connection := server.Connection
		target := connection.ThroughputTarget
		downLanes, upLanes := c.cfg.TransferStreams.Lanes(target.Protocol, target.Transport)
		hc, closeHTTP := protocolClient(server.credential, connection.ThroughputTarget.Protocol)
		// Upload lanes get their own connection so control requests and download reads never queue behind them.
		up, closeUp := protocolClient(server.credential, connection.ThroughputTarget.Protocol)
		ws, closeWS := websocketClient(server.credential)
		defer closeHTTP()
		defer closeUp()
		defer closeWS()
		r := &runner{
			cfg:           c.cfg,
			cred:          server.credential,
			streams:       byDirection[int]{downLanes, upLanes},
			http:          hc,
			uploadHTTP:    up,
			websocketHTTP: ws,
			target:        new(connection.ThroughputTarget),
			latencyTarget: connection.LatencyTarget,
			coordinated:   &participantCounters{},
			idleRTT:       connection.WarmRTT,
			teardown:      teardown,
		}
		r.emit = func(e Event) {
			e.ServerID = server.Server.ID
			c.emit(e)
		}
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
	details := &RunDetails{
		Participants:     c.ids(),
		Intervals:        slices.Clone(c.aggregate.intervals),
		OmittedIntervals: c.aggregate.omitted,
		Failures:         slices.Clone(c.failures),
		Outcome:          outcome,
	}
	if c.prepared != nil {
		details.LatencyFocus = c.prepared.LatencyFocus
	}
	for _, server := range c.servers {
		summary := ServerRunSummary{Server: server.prepared.Server, Results: slices.Clone(server.results)}
		if connection := server.prepared.Connection; connection != nil {
			summary.Throughput, summary.LatencyTarget = connection.ThroughputTarget, connection.LatencyTarget
		}
		details.Servers = append(details.Servers, summary)
	}
	return details
}

func (c *coordinator) publish() {
	c.emit(Event{Kind: EventServers, At: time.Now(), Servers: c.details(OutcomeRunning)})
}

func (c *coordinator) run(ctx context.Context) error {
	c.publish()
	plan := c.cfg.Plan()
	for i, stage := range plan {
		if err := c.stage(ctx, stage, i < len(plan)-1); err != nil && !errors.Is(err, errStageSkipped) {
			return err
		}
		c.emit(Event{Kind: EventStage, At: time.Now(), Stage: stage.Name, Phase: PhaseFinished})
		c.publish()
	}
	return nil
}

func (c *coordinator) outcome(ctx context.Context, err error) Outcome {
	switch {
	case err == nil && c.missingResults():
		return OutcomeIncomplete
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

func (c *coordinator) missingResults() bool {
	for _, stage := range c.cfg.Plan() {
		replied := func(r Result) bool { return r.Stage == stage.Name && r.Direction == "" && r.Latency.Count > 0 }
		for _, p := range c.active() {
			if len(stage.Directions) == 0 && !slices.ContainsFunc(p.results, replied) {
				return true
			}
		}
	}
	return c.unavailable
}

func (c *coordinator) noSurvivors() error {
	if len(c.failures) == 0 {
		return errNoSurvivors
	}
	return fmt.Errorf("%w: %w", errNoSurvivors, c.failures[len(c.failures)-1].Err)
}

func (c *coordinator) failure(server *stageServer, stage StagePlan, role string, err error, at time.Time) {
	scope := ScopeThroughput
	if role == roleLatency {
		scope = ScopeLatency
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
	failure := ServerFailure{
		ServerID: server.id(),
		Stage:    stage.Name,
		Scope:    scope,
		Reason:   failureReason(err),
		Err:      err,
		At:       at.Sub(c.started),
	}
	c.failures = append(c.failures, failure)
	c.emit(Event{
		Kind:     EventServerFailure,
		At:       at,
		Stage:    stage.Name,
		ServerID: failure.ServerID,
		Failure:  new(failure),
	})
	c.publish()
}

func (c *coordinator) retainLatency(outcome resourceOutcome, normalEnd bool) {
	if outcome.role != roleLatency {
		return
	}
	result := outcome.result
	if normalEnd && outcome.err == context.Canceled {
		result.Err = nil
	}
	p := outcome.server.participant
	p.results = append(p.results, result)
	if result.Stage == StageLatency && result.Latency.P50 > 0 {
		p.transport.idleRTT = result.Latency.P50
	}
}
