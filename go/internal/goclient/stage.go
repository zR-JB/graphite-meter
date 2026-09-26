package goclient

import (
	"context"
	"errors"
	"fmt"
	"slices"
	"sync"
	"time"
)

const roleLatency = "latency"

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
	misses   map[string]error
	epoch    int
	final    bool
}

type stageRun struct {
	c         *coordinator
	plan      StagePlan
	ctx       context.Context
	cancel    context.CancelCauseFunc
	roles     []string
	servers   []*stageServer
	gates     []*stageGate
	reports   chan readyResource
	outcomes  chan resourceOutcome
	start     chan struct{}
	work      sync.WaitGroup
	seen      map[readyResource]bool
	measuring bool

	samples      chan sampledBoundary
	sampling     sync.WaitGroup
	stopSample   context.CancelFunc
	epoch        int
	inFlight     bool
	ending       bool
	lastBytes    map[string]byDirection[uint64]
	lastMovement map[string]*byDirection[time.Time]
	misses       map[string]int
}

func (c *coordinator) stage(ctx context.Context, plan StagePlan, handover bool) (err error) {
	s := c.openStage(ctx, plan)
	defer func() { s.close(err, handover) }()
	if err := s.ready(); err != nil {
		return err
	}
	if err := s.warmup(); err != nil {
		return err
	}
	if err := s.window(); err != nil || !s.transfer() {
		return err
	}
	return s.final()
}

func (c *coordinator) openStage(ctx context.Context, plan StagePlan) *stageRun {
	stageCtx, cancel := context.WithCancelCause(ctx)
	s := &stageRun{c: c, plan: plan, ctx: stageCtx, cancel: cancel, start: make(chan struct{}),
		seen: map[readyResource]bool{}, samples: make(chan sampledBoundary, 1)}
	for _, dir := range plan.Directions {
		s.roles = append(s.roles, string(dir))
	}
	if !s.transfer() || c.cfg.LoadedLatency {
		s.roles = append(s.roles, roleLatency)
	}
	s.reports = make(chan readyResource, len(c.active())*len(s.roles))
	s.outcomes = make(chan resourceOutcome, cap(s.reports))
	c.emit(Event{Kind: EventStage, At: time.Now(), Stage: plan.Name, Phase: PhasePreparing})
	for _, p := range c.active() {
		server := &stageServer{participant: p}
		transferCtx, cancelTransfer := context.WithCancelCause(stageCtx)
		latencyCtx, cancelLatency := context.WithCancelCause(stageCtx)
		server.cancelTransfer, server.cancelLatency = cancelTransfer, cancelLatency
		s.servers = append(s.servers, server)
		p.transport.coordinated = &participantCounters{}
		for _, role := range s.roles {
			own, ownCancel := transferCtx, cancelTransfer
			if role == roleLatency {
				own, ownCancel = latencyCtx, cancelLatency
			}
			gate := &stageGate{
				cancel:      ownCancel,
				start:       s.start,
				reportReady: func() { s.reports <- readyResource{p.id(), role} },
			}
			s.gates = append(s.gates, gate)
			s.work.Go(func() { s.outcomes <- server.measure(own, plan, role, gate) })
		}
	}
	return s
}

func (s *stageRun) transfer() bool { return len(s.plan.Directions) > 0 }

func (s *stageRun) close(err error, handover bool) {
	if s.stopSample != nil {
		s.stopSample()
	}
	s.sampling.Wait()
	if err == nil {
		end := context.Canceled
		if handover {
			end = errHandover
		}
		for _, server := range s.servers {
			server.cancelTransfer(end)
		}
		s.work.Wait()
	}
	s.cancel(err)
	s.work.Wait()
	close(s.outcomes)
	for outcome := range s.outcomes {
		s.c.retainLatency(outcome, err == nil)
	}
	if s.transfer() && s.measuring {
		s.finish(err)
	}
}

func (s *stageRun) missing(server *stageServer) []string {
	var out []string
	for _, role := range s.roles {
		gone := server.removed || role == roleLatency && server.latencyFailed
		if !gone && !s.seen[readyResource{server.id(), role}] {
			out = append(out, role)
		}
	}
	return out
}

func (s *stageRun) fail(server *stageServer, role string, err error, at time.Time) {
	s.c.failure(server, s.plan, role, err, at)
}

func (s *stageRun) handle(outcome resourceOutcome) error {
	c := s.c
	c.retainLatency(outcome, false)
	if outcome.err == nil ||
		s.ctx.Err() != nil ||
		outcome.server.removed ||
		outcome.role == roleLatency && outcome.server.latencyFailed {
		return nil
	}
	if !c.hasMeasured {
		name := outcome.server.prepared.Server.Name
		return fmt.Errorf("%s: %w; resolve the selection before starting", name, outcome.err)
	}
	before := len(c.ids())
	s.fail(outcome.server, outcome.role, outcome.err, outcome.at)
	switch {
	case len(c.ids()) == 0:
		if s.measuring && s.transfer() {
			c.aggregate.restart(nil, time.Since(c.started), ReasonDropout)
		}
		return fmt.Errorf("%w: %w", errNoSurvivors, outcome.err)
	case s.measuring && s.transfer() && len(c.ids()) != before:
		s.reset()
	}
	return nil
}

func (s *stageRun) ready() error {
	timer := time.NewTimer(stageReadyTimeout)
	defer timer.Stop()
	for slices.ContainsFunc(s.servers, func(server *stageServer) bool { return len(s.missing(server)) > 0 }) {
		select {
		case <-s.ctx.Done():
			return context.Cause(s.ctx)
		case resource := <-s.reports:
			s.seen[resource] = true
		case outcome := <-s.outcomes:
			if err := s.handle(outcome); err != nil {
				return err
			}
		case now := <-timer.C:
			failure := fmt.Errorf("server resources were not ready within %v: %w", stageReadyTimeout,
				context.DeadlineExceeded)
			if !s.c.hasMeasured {
				return failure
			}
			for _, server := range s.servers {
				for _, role := range s.missing(server) {
					s.fail(server, role, failure, now)
				}
			}
			if len(s.c.ids()) == 0 {
				return s.c.noSurvivors()
			}
		}
	}
	return nil
}

func (s *stageRun) warmup() error {
	warmup := s.c.cfg.Warmup
	for _, server := range s.servers {
		if !server.removed {
			warmup = max(warmup, adaptiveWarmup(s.c.cfg.Warmup, server.transport.idleRTT))
		}
	}
	if warmup > 0 {
		s.c.emit(Event{Kind: EventStage, At: time.Now(), Stage: s.plan.Name, Phase: PhaseWarmup})
	}
	timer := time.NewTimer(warmup)
	defer timer.Stop()
	for {
		select {
		case <-s.ctx.Done():
			return context.Cause(s.ctx)
		case outcome := <-s.outcomes:
			if err := s.handle(outcome); err != nil {
				return err
			}
		case <-timer.C:
			return nil
		}
	}
}

func (s *stageRun) window() error {
	started, initial, err := s.open()
	if err != nil {
		return err
	}
	for _, gate := range s.gates {
		gate.boundaryStart = started
	}
	s.measuring, s.c.hasMeasured = true, true
	s.c.emit(Event{Kind: EventStage, At: started, Stage: s.plan.Name, Phase: PhaseMeasuring})
	close(s.start)
	end := time.NewTimer(time.Until(started.Add(s.plan.Duration)))
	defer end.Stop()
	var tick <-chan time.Time
	if s.transfer() {
		ticker := time.NewTicker(sampleInterval)
		defer ticker.Stop()
		tick = ticker.C
		s.beginSampling(started, initial)
	}
	for {
		select {
		case <-s.ctx.Done():
			return context.Cause(s.ctx)
		case outcome := <-s.outcomes:
			if err := s.handle(outcome); err != nil {
				return err
			}
		case <-tick:
			s.sample(false)
		case sample := <-s.samples:
			if _, err := s.observe(sample); err != nil {
				return err
			}
		case <-end.C:
			return nil
		}
	}
}

func (s *stageRun) final() error {
	s.ending = true
	s.sample(true)
	for {
		select {
		case <-s.ctx.Done():
			return context.Cause(s.ctx)
		case outcome := <-s.outcomes:
			if err := s.handle(outcome); err != nil {
				return err
			}
		case sample := <-s.samples:
			if done, err := s.observe(sample); done {
				return err
			}
		}
	}
}

func (s *stageRun) open() (time.Time, measurementBoundary, error) {
	c := s.c
	initial, _ := s.collect(s.ctx, c.active(), checkpointBudget)
	if slices.Contains(s.plan.Directions, Up) {
		for _, server := range s.servers {
			if !server.removed && initial.up[server.id()] == nil {
				failure := errors.New("receiver checkpoint unavailable before measurement")
				if !c.hasMeasured {
					return time.Time{}, initial, fmt.Errorf("%s: %w", server.prepared.Server.Name, failure)
				}
				s.fail(server, string(Up), failure, time.Now())
			}
		}
		if len(c.ids()) == 0 {
			return time.Time{}, initial, c.noSurvivors()
		}
	}
	for drained := false; !drained; {
		select {
		case outcome := <-s.outcomes:
			if err := s.handle(outcome); err != nil {
				return time.Time{}, initial, err
			}
		default:
			drained = true
		}
	}
	started := time.Now()
	initial.at = started.Sub(c.started)
	for _, p := range c.active() {
		initial.down[p.id()] = p.transport.coordinated.down.Load()
	}
	if s.transfer() {
		c.aggregate.beginStage(s.plan.Name, c.ids(), initial.at)
		c.aggregate.observe(initial)
	}
	return started, initial, nil
}

func (s *stageRun) beginSampling(started time.Time, initial measurementBoundary) {
	s.lastBytes, s.misses = map[string]byDirection[uint64]{}, map[string]int{}
	s.lastMovement = map[string]*byDirection[time.Time]{}
	for _, p := range s.c.active() {
		bytes := byDirection[uint64]{down: initial.down[p.id()]}
		if snapshot := initial.up[p.id()]; snapshot != nil {
			bytes.up = snapshot.Bytes
		}
		s.lastBytes[p.id()] = bytes
		s.lastMovement[p.id()] = &byDirection[time.Time]{started, started}
	}
}

func (s *stageRun) sample(final bool) {
	if s.inFlight {
		return
	}
	s.inFlight = true
	ctx, cancel := context.WithCancel(s.ctx)
	s.stopSample = cancel
	participants, epoch := s.c.active(), s.epoch
	budget := checkpointBudget
	if final {
		budget = finalCheckpointBudget
	}
	s.sampling.Go(func() {
		defer cancel()
		boundary, misses := s.collect(ctx, participants, budget)
		s.samples <- sampledBoundary{boundary, misses, epoch, final}
	})
}

func (s *stageRun) reset() {
	s.epoch++
	if s.stopSample != nil {
		s.stopSample()
	}
	s.c.aggregate.restart(s.c.ids(), time.Since(s.c.started), ReasonDropout)
	s.emitRates(nil)
	s.sample(s.ending)
}

func (s *stageRun) dropsServer(id string, err error, final bool) bool {
	if err == nil {
		s.misses[id] = 0
		return false
	}
	s.misses[id]++
	_, auth := errors.AsType[*AuthRequiredError](err)
	return auth || s.misses[id] >= 3 && !final
}

func (s *stageRun) observe(sample sampledBoundary) (bool, error) {
	c := s.c
	s.inFlight = false
	if sample.epoch != s.epoch {
		s.sample(s.ending)
		return false, nil
	}
	removed, stalled := false, false
	for _, server := range s.servers {
		if server.removed {
			continue
		}
		id := server.id()
		if err := sample.misses[id]; s.dropsServer(id, err, sample.final) {
			s.fail(server, string(Up), err, time.Now())
			removed = true
			continue
		}
		bytes := byDirection[uint64]{down: sample.boundary.down[id]}
		if snapshot := sample.boundary.up[id]; snapshot != nil {
			bytes.up = snapshot.Bytes
		} else {
			bytes.up = sample.boundary.observedUp[id].maximum
		}
		for _, dir := range s.plan.Directions {
			switch {
			case bytes.of(dir) > s.lastBytes[id].of(dir):
				s.lastMovement[id].set(dir, time.Now())
			case sample.final:
				stalled = true
			case !s.ending && time.Since(s.lastMovement[id].of(dir)) >= redialWindow:
				err := fmt.Errorf("%s %w for %v", dir, errStalled, redialWindow)
				s.fail(server, string(dir), err, time.Now())
				removed = true
			}
		}
		s.lastBytes[id] = bytes
	}
	// A final boundary where a direction stood still ends the result at the last good boundary.
	if stalled {
		c.aggregate.credit(sample.boundary)
	} else if window, restarted := c.aggregate.observe(sample.boundary); window != nil || restarted {
		s.emitRates(window)
	}
	switch {
	case len(c.ids()) == 0:
		c.aggregate.restart(nil, time.Since(c.started), ReasonDropout)
		return true, c.noSurvivors()
	case removed:
		s.reset()
	case sample.final:
		return true, nil
	case s.ending:
		s.sample(true)
	}
	return false, nil
}

func (s *stageRun) collect(
	ctx context.Context,
	servers []*participant,
	budget time.Duration,
) (measurementBoundary, map[string]error) {
	boundary := measurementBoundary{
		at:         time.Since(s.c.started),
		down:       map[string]uint64{},
		up:         map[string]*ReceiverSnapshot{},
		observedUp: map[string]uploadLedger{},
	}
	for _, server := range servers {
		boundary.down[server.id()] = server.transport.coordinated.down.Load()
		if id, bytes := server.transport.coordinated.uploaded(); id != "" {
			boundary.observedUp[server.id()] = uploadLedger{id, bytes}
		}
	}
	if !slices.Contains(s.plan.Directions, Up) {
		return boundary, nil
	}
	ctx, cancel := context.WithTimeout(ctx, budget)
	defer cancel()
	snapshots := make([]*ReceiverSnapshot, len(servers))
	errs := make([]error, len(servers))
	var work sync.WaitGroup
	for i, server := range servers {
		work.Go(func() { snapshots[i], errs[i] = server.transport.receiverCheckpoint(ctx) })
	}
	work.Wait()
	misses := map[string]error{}
	for i, server := range servers {
		boundary.up[server.id()] = snapshots[i]
		if errs[i] != nil {
			misses[server.id()] = errs[i]
		}
	}
	return boundary, misses
}

func (s *stageRun) emitRates(window *AggregateWindow) {
	for _, dir := range s.plan.Directions {
		sample := ThroughputSample{Unavailable: true}
		if window != nil {
			_, rate := window.direction(dir)
			if rate == nil {
				continue
			}
			sample = ThroughputSample{BytesPerSec: *rate, TotalBytes: s.c.aggregate.total(dir)}
		}
		s.c.emit(Event{Kind: EventThroughput, At: time.Now(), Stage: s.plan.Name, Direction: dir, Throughput: sample})
	}
}

func (s *stageRun) finish(stageErr error) {
	c := s.c
	for _, dir := range s.plan.Directions {
		result := c.aggregate.result(dir)
		c.unavailable = c.unavailable || result.Unavailable
		if stageErr != nil {
			result.Err = stageErr
		}
		c.emit(Event{Kind: EventResult, At: time.Now(), Stage: s.plan.Name, Direction: dir, Result: new(result)})
		for _, server := range c.servers {
			own := c.aggregate.serverResult(server.id(), dir)
			if server.removed {
				own.Err = errors.New("earlier partial measurement")
			}
			server.results = append(server.results, own)
		}
	}
}

func (s *stageServer) measure(ctx context.Context, stage StagePlan, role string, gate *stageGate) resourceOutcome {
	outcome := resourceOutcome{server: s, role: role}
	if role == roleLatency {
		stats, err := s.transport.measureLatency(ctx, stage.Name, len(stage.Directions) > 0, stage.Duration, gate)
		outcome.result = Result{Stage: stage.Name, Latency: stats, Elapsed: stats.Elapsed, Err: err}
		outcome.err = err
	} else if Direction(role) == Down {
		outcome.err = s.transport.measureDownload(ctx, gate)
	} else {
		outcome.err = s.transport.measureUpload(ctx, gate)
	}
	outcome.at = time.Now()
	return outcome
}
