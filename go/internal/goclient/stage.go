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
}

// sampler collects one interval's boundaries on its own clock; a restart replaces it.
type sampler struct {
	results chan sampledBoundary
	finish  chan struct{}
	cancel  context.CancelFunc
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

	sampler      *sampler
	sampling     sync.WaitGroup
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
	if len(c.servers) == 1 && c.servers[0].transport != nil {
		c.servers[0].removed = false
	}
	stageCtx, cancel := context.WithCancelCause(ctx)
	s := &stageRun{c: c, plan: plan, ctx: stageCtx, cancel: cancel, start: make(chan struct{}),
		seen: map[readyResource]bool{}}
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
	if s.sampler != nil {
		s.sampler.cancel()
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
	s.c.failure(server, s.plan, role, err, at, !s.measuring)
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
	before := len(c.ids())
	s.fail(outcome.server, outcome.role, outcome.err, outcome.at)
	if err := s.lost(); err != nil {
		return err
	}
	if s.measuring && s.transfer() && len(c.ids()) != before {
		s.reset()
	}
	return nil
}

// lost ends the stage once no server is left in it; a sole server that measured before retries next stage.
func (s *stageRun) lost() error {
	c := s.c
	if len(c.ids()) > 0 {
		return nil
	}
	if s.measuring && s.transfer() {
		c.aggregate.restart(nil, time.Since(c.started), ReasonDropout)
	}
	if c.hasMeasured && len(c.servers) == 1 {
		return fmt.Errorf("%w: %w", errStageSkipped, c.failures[len(c.failures)-1].Err)
	}
	return c.noSurvivors()
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
			for _, server := range s.servers {
				for _, role := range s.missing(server) {
					s.fail(server, role, failure, now)
				}
			}
			if err := s.lost(); err != nil {
				return err
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
	if s.transfer() {
		s.beginSampling(started, initial)
		s.startSampler()
	}
	for {
		select {
		case <-s.ctx.Done():
			return context.Cause(s.ctx)
		case outcome := <-s.outcomes:
			if err := s.handle(outcome); err != nil {
				return err
			}
		case sample := <-s.results():
			if _, err := s.observe(sample); err != nil {
				return err
			}
		case <-end.C:
			return nil
		}
	}
}

func (s *stageRun) results() <-chan sampledBoundary {
	if s.sampler == nil {
		return nil
	}
	return s.sampler.results
}

func (s *stageRun) final() error {
	s.ending = true
	close(s.sampler.finish)
	for {
		select {
		case <-s.ctx.Done():
			return context.Cause(s.ctx)
		case outcome := <-s.outcomes:
			if err := s.handle(outcome); err != nil {
				return err
			}
		case sample := <-s.results():
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
				s.fail(server, string(Up), errors.New("receiver checkpoint unavailable before measurement"), time.Now())
			}
		}
		if err := s.lost(); err != nil {
			return time.Time{}, initial, err
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

// startSampler collects a boundary every tick and, once finished, a last one on the final budget. A tick read
// late means the client itself stalled, so that boundary resumes evidence.
func (s *stageRun) startSampler() {
	ctx, cancel := context.WithCancel(s.ctx)
	own := &sampler{results: make(chan sampledBoundary), finish: make(chan struct{}), cancel: cancel}
	s.sampler = own
	participants := s.c.active()
	send := func(sample sampledBoundary) bool {
		select {
		case own.results <- sample:
			return true
		case <-ctx.Done():
			return false
		}
	}
	s.sampling.Go(func() {
		ticker := time.NewTicker(sampleInterval)
		defer ticker.Stop()
		for {
			select {
			case <-ctx.Done():
				return
			case <-own.finish:
				boundary, misses := s.collect(ctx, participants, finalCheckpointBudget)
				boundary.final = true
				send(sampledBoundary{boundary, misses})
				return
			case tick := <-ticker.C:
				stalled := time.Since(tick) > clientStall
				boundary, misses := s.collect(ctx, participants, checkpointBudget)
				boundary.stalled = stalled
				if !send(sampledBoundary{boundary, misses}) {
					return
				}
			}
		}
	})
}

func (s *stageRun) reset() {
	s.sampler.cancel()
	s.c.aggregate.restart(s.c.ids(), time.Since(s.c.started), ReasonDropout)
	s.emitRates(nil)
	s.startSampler()
	if s.ending {
		close(s.sampler.finish)
	}
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
	removed, final := false, sample.boundary.final
	for _, server := range s.servers {
		if server.removed {
			continue
		}
		id := server.id()
		if err := sample.misses[id]; s.dropsServer(id, err, final) {
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
			case !s.ending && time.Since(s.lastMovement[id].of(dir)) >= redialWindow:
				err := fmt.Errorf("%s %w for %v", dir, errStalled, redialWindow)
				s.fail(server, string(dir), err, time.Now())
				removed = true
			}
		}
		s.lastBytes[id] = bytes
	}
	if window, restarted := c.aggregate.observe(sample.boundary); window != nil || restarted {
		s.emitRates(window)
	}
	if err := s.lost(); err != nil {
		return true, err
	}
	switch {
	case removed:
		s.reset()
	case final:
		return true, nil
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
				own.Err = c.departure(server.id(), s.plan.Name)
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
