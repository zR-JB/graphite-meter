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

type ServerRunSummary struct {
	Server                     wire.ServerEntry
	Throughput                 wire.ThroughputTarget
	LatencyTarget              *wire.LatencyTarget
	Results                    []Result
	TotalDownload, TotalUpload uint64
}

type RunDetails struct {
	Selection        []wire.ServerEntry
	Participants     []string
	LatencyFocus     string
	Servers          []ServerRunSummary
	Intervals        []AggregationInterval
	OmittedIntervals int
	Failures         []ServerFailure
	Outcome          string
}

type nativeParticipant struct {
	prepared  PreparedServer
	transport *runner
	removed   bool
	results   []Result
	close     func()
}
type stageParticipant struct {
	participant                   *nativeParticipant
	cancelTransfer, cancelLatency context.CancelCauseFunc
	latencyFailed                 bool
}
type readyResource struct{ serverID, role string }
type resourceOutcome struct {
	server *stageParticipant
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

type nativeCoordinator struct {
	cfg         Config
	prepared    *PreparedRun
	servers     []*nativeParticipant
	streams     map[string]map[string]streamCounts
	aggregate   aggregateMeasurements
	failures    []ServerFailure
	started     time.Time
	hasMeasured bool
	emit        func(Event)
}

var errNoSurvivors = errors.New("all selected servers failed")

// RunSelection has one stage schedule. Its participants own connections and credentials, never independent runs.
func RunSelection(ctx context.Context, cfg Config, prepared *PreparedRun, emit func(Event)) (err error) {
	defer func() { emit(Event{Kind: EventDone, At: time.Now(), Err: err}) }()
	cfg = cfg.normalized()
	if prepared == nil || !prepared.Ready() {
		return errors.New("resolve every selected server before starting")
	}
	streams, err := planRunStreams(cfg, prepared.Servers)
	if err != nil {
		return err
	}
	c := &nativeCoordinator{cfg: cfg, prepared: prepared, streams: streams, started: time.Now(), emit: emit}
	for _, server := range prepared.Servers {
		own := server.config
		own.Stages = cfg.Stages
		own.Warmup = cfg.Warmup
		own.LatencyDuration = cfg.LatencyDuration
		own.DownloadDuration = cfg.DownloadDuration
		own.UploadDuration = cfg.UploadDuration
		own.BidirectionalDuration = cfg.BidirectionalDuration
		connection := server.Connection
		hc, closeHTTP := protocolClient(own, connection.ThroughputTarget.Protocol, func() *http.Transport { return baseTransport(own) })
		ws, closeWS := websocketClient(own)
		transport := &runner{cfg: own, http: hc, websocketHTTP: ws, target: new(connection.ThroughputTarget), latencyTarget: connection.LatencyTarget, coordinated: &participantCounters{}, idleRTT: connection.PreflightRTT}
		transport.emit = func(e Event) { e.ServerID = server.Server.ID; emit(e) }
		participant := &nativeParticipant{prepared: server, transport: transport, close: func() { closeHTTP(); closeWS() }}
		c.servers = append(c.servers, participant)
		protocol := targetProtocolEvidence(connection.ThroughputTarget.Protocol)
		if connection.ThroughputTarget.Protocol == "negotiated" {
			protocol = connection.Probe.ProtocolNegotiated
		}
		event := Event{Kind: EventPreflight, At: time.Now(), ServerID: server.Server.ID, Preflight: new(connection.Preflight), Probe: new(connection.Probe), LatencyProbe: connection.LatencyProbe, ThroughputTarget: connection.ThroughputTarget.ID, ThroughputTransport: connection.ThroughputTarget.Transport, ThroughputProtocol: protocol}
		if connection.LatencyTarget != nil {
			event.LatencyTarget = connection.LatencyTarget.ID
			event.LatencyTransport = connection.LatencyTarget.Transport
			event.LatencyProtocol = latencyBusEvidence(connection.LatencyTarget, connection.LatencyProbe)
		}
		emit(event)
		defer participant.close()
	}
	return c.run(ctx)
}

func (c *nativeCoordinator) active() []*nativeParticipant {
	return slices.DeleteFunc(slices.Clone(c.servers), func(s *nativeParticipant) bool { return s.removed })
}
func (c *nativeCoordinator) ids() []string {
	ids := []string{}
	for _, server := range c.servers {
		if !server.removed {
			ids = append(ids, server.prepared.Server.ID)
		}
	}
	return ids
}
func (c *nativeCoordinator) details(outcome string) *RunDetails {
	details := &RunDetails{Participants: c.ids(), LatencyFocus: c.prepared.LatencyFocus, Intervals: slices.Clone(c.aggregate.intervals), OmittedIntervals: c.aggregate.omitted, Failures: slices.Clone(c.failures), Outcome: outcome}
	for _, server := range c.servers {
		identity := server.prepared.Server
		details.Selection = append(details.Selection, identity)
		total := c.aggregate.totals[identity.ID]
		details.Servers = append(details.Servers, ServerRunSummary{Server: identity, Throughput: server.prepared.Connection.ThroughputTarget, LatencyTarget: server.prepared.Connection.LatencyTarget, Results: slices.Clone(server.results), TotalDownload: total.down, TotalUpload: total.up})
	}
	return details
}
func (c *nativeCoordinator) publish(outcome string) {
	c.emit(Event{Kind: EventServers, At: time.Now(), Servers: c.details(outcome)})
}
func (c *nativeCoordinator) run(ctx context.Context) error {
	c.publish("running")
	var err error
	for _, stage := range c.cfg.Plan() {
		if err = c.stage(ctx, stage); err != nil {
			break
		}
		c.emit(Event{Kind: EventStage, At: time.Now(), Stage: stage.Name, Phase: StageFinished})
	}
	outcome := "complete"
	if len(c.failures) > 0 {
		outcome = "partial"
	}
	if err != nil {
		outcome = "incomplete"
	}
	c.publish(outcome)
	return err
}

func (c *nativeCoordinator) failure(server *stageParticipant, stage StagePlan, role string, err error, at time.Time) {
	scope := "throughput"
	if role == "latency" {
		scope = "latency"
		if server.latencyFailed {
			return
		}
		server.latencyFailed = true
		server.cancelLatency(err)
	} else {
		if server.participant.removed {
			return
		}
		server.participant.removed = true
		server.cancelTransfer(err)
		server.cancelLatency(err)
	}
	failure := ServerFailure{ServerID: server.participant.prepared.Server.ID, Stage: stage.Name, Scope: scope, Reason: "connection-lost", Message: err.Error(), At: at.Sub(c.started)}
	if _, ok := errors.AsType[*AuthRequiredError](err); ok {
		failure.Reason = "authentication-required"
	}
	c.failures = append(c.failures, failure)
	c.emit(Event{Kind: EventServerFailure, At: at, Stage: stage.Name, ServerID: failure.ServerID, Failure: new(failure)})
}
func (c *nativeCoordinator) retainLatency(outcome resourceOutcome, normalEnd bool) {
	if outcome.role != "latency" {
		return
	}
	result := outcome.result
	if normalEnd && errors.Is(outcome.err, context.Canceled) {
		result.Err = nil
	}
	p := outcome.server.participant
	// One final population per server and stage, even when cleanup follows a failure.
	for i, old := range p.results {
		if old.Stage == result.Stage && old.Direction == "" {
			p.results[i] = result
			return
		}
	}
	p.results = append(p.results, result)
	if result.Stage == "latency" && result.Latency.P50 > 0 {
		p.transport.idleRTT = result.Latency.P50
	}
	c.emit(Event{Kind: EventResult, At: time.Now(), Stage: result.Stage, ServerID: p.prepared.Server.ID, Result: new(result)})
}

func (c *nativeCoordinator) stage(ctx context.Context, stage StagePlan) (stageErr error) {
	stageCtx, cancel := context.WithCancelCause(ctx)
	var work sync.WaitGroup
	ready := make(chan readyResource, 12)
	outcomes := make(chan resourceOutcome, 12)
	start := make(chan struct{})
	var servers []*stageParticipant
	var gates []*stageGate
	measuring := false
	normalEnd := false
	c.emit(Event{Kind: EventStage, At: time.Now(), Stage: stage.Name, Phase: StagePreparing})
	for _, participant := range c.active() {
		transferCtx, cancelTransfer := context.WithCancelCause(stageCtx)
		latencyCtx, cancelLatency := context.WithCancelCause(stageCtx)
		server := &stageParticipant{participant: participant, cancelTransfer: cancelTransfer, cancelLatency: cancelLatency}
		servers = append(servers, server)
		r := participant.transport
		r.coordinated = &participantCounters{}
		r.streams = c.streams[stage.Name][participant.prepared.Server.ID]
		launch := func(role string, ownCtx context.Context, ownCancel context.CancelCauseFunc) {
			gate := &stageGate{ctx: ownCtx, cancel: ownCancel, start: start, reportReady: func() { ready <- readyResource{participant.prepared.Server.ID, role} }}
			gates = append(gates, gate)
			work.Go(func() {
				var result Result
				var err error
				if role == "latency" {
					stats, failure := r.measureLatency(ownCtx, stage.Name, len(stage.Directions) > 0, stage.Duration, gate)
					err = failure
					result = Result{Stage: stage.Name, Latency: stats, Samples: stats.Count, Elapsed: stats.Elapsed, Err: failure}
				} else {
					result, err = r.measureDirection(ownCtx, stage.Name, Direction(role), stage.Duration, gate)
				}
				outcomes <- resourceOutcome{server: server, role: role, result: result, err: err, at: time.Now()}
			})
		}
		for _, dir := range stage.Directions {
			launch(string(dir), transferCtx, cancelTransfer)
		}
		if len(stage.Directions) == 0 || c.cfg.LoadedLatency {
			launch("latency", latencyCtx, cancelLatency)
		}
	}
	defer func() {
		cancel(stageErr)
		work.Wait()
		close(outcomes)
		for outcome := range outcomes {
			c.retainLatency(outcome, normalEnd)
		}
		if len(stage.Directions) > 0 && measuring {
			c.finishTransferStage(stage, stageErr)
		}
	}()
	seen := map[readyResource]bool{}
	allReady := func() bool {
		for _, server := range servers {
			if server.participant.removed {
				continue
			}
			id := server.participant.prepared.Server.ID
			for _, dir := range stage.Directions {
				if !seen[readyResource{id, string(dir)}] {
					return false
				}
			}
			if (len(stage.Directions) == 0 || c.cfg.LoadedLatency) && !server.latencyFailed && !seen[readyResource{id, "latency"}] {
				return false
			}
		}
		return true
	}
	handle := func(outcome resourceOutcome) error {
		c.retainLatency(outcome, false)
		if outcome.err == nil || ctx.Err() != nil {
			return nil
		}
		if outcome.server.participant.removed || outcome.role == "latency" && outcome.server.latencyFailed {
			return nil
		}
		if !c.hasMeasured {
			return fmt.Errorf("%s: %w; resolve the selection before starting", outcome.server.participant.prepared.Server.Name, outcome.err)
		}
		c.failure(outcome.server, stage, outcome.role, outcome.err, outcome.at)
		if len(c.ids()) == 0 {
			if measuring && len(stage.Directions) > 0 {
				c.aggregate.begin(stage.Name, nil, time.Since(c.started), "dropout")
			}
			return fmt.Errorf("%w: %w", errNoSurvivors, outcome.err)
		}
		return nil
	}
	prepareTimer := time.NewTimer(stageReadyTimeout)
	defer prepareTimer.Stop()
	for !allReady() {
		select {
		case <-ctx.Done():
			return context.Cause(ctx)
		case <-prepareTimer.C:
			failure := fmt.Errorf("server resources were not ready within %v", stageReadyTimeout)
			if !c.hasMeasured {
				return failure
			}
			for _, server := range servers {
				if server.participant.removed {
					continue
				}
				id := server.participant.prepared.Server.ID
				for _, dir := range stage.Directions {
					if !seen[readyResource{id, string(dir)}] {
						c.failure(server, stage, string(dir), failure, time.Now())
						break
					}
				}
				if !server.participant.removed && !server.latencyFailed && (len(stage.Directions) == 0 || c.cfg.LoadedLatency) && !seen[readyResource{id, "latency"}] {
					c.failure(server, stage, "latency", failure, time.Now())
				}
			}
			if len(c.ids()) == 0 {
				return errNoSurvivors
			}
		case resource := <-ready:
			seen[resource] = true
		case outcome := <-outcomes:
			if err := handle(outcome); err != nil {
				return err
			}
		}
	}
	prepareTimer.Stop()
	warmup := c.cfg.Warmup
	for _, server := range servers {
		if !server.participant.removed {
			warmup = max(warmup, adaptiveWarmup(c.cfg.Warmup, server.participant.transport.idleRTT))
		}
	}
	if warmup > 0 {
		c.emit(Event{Kind: EventStage, At: time.Now(), Stage: stage.Name, Phase: StageWarmup})
		timer := time.NewTimer(warmup)
		defer timer.Stop()
	warm:
		for {
			select {
			case <-ctx.Done():
				return context.Cause(ctx)
			case <-timer.C:
				break warm
			case outcome := <-outcomes:
				if err := handle(outcome); err != nil {
					return err
				}
			}
		}
	}
	// All upload baselines must be available before the first measured interval opens.
	initial := c.capture(stageCtx, stage, c.active())
	if len(stage.Directions) > 0 && stage.Name != "download" {
		for _, server := range servers {
			if !server.participant.removed && initial.up[server.participant.prepared.Server.ID] == nil {
				failure := errors.New("receiver checkpoint unavailable before measurement")
				if !c.hasMeasured {
					return fmt.Errorf("%s: %w", server.participant.prepared.Server.Name, failure)
				}
				c.failure(server, stage, "upload", failure, time.Now())
			}
		}
		if len(c.ids()) == 0 {
			return errNoSurvivors
		}
	}
drain:
	for {
		select {
		case outcome := <-outcomes:
			if err := handle(outcome); err != nil {
				return err
			}
		default:
			break drain
		}
	}
	// Checkpoint replies finish preparation. The client populations begin only
	// now; receiver baselines keep their original request/response brackets.
	started := time.Now()
	initial.at = started.Sub(c.started)
	for _, server := range c.active() {
		initial.down[server.prepared.Server.ID] = server.transport.coordinated.download()
	}
	if len(stage.Directions) > 0 {
		c.aggregate.begin(stage.Name, c.ids(), initial.at, "stage-start")
		c.aggregate.observe(initial)
	}
	for _, gate := range gates {
		gate.boundaryStart = started
	}
	measuring = true
	c.hasMeasured = true
	c.emit(Event{Kind: EventStage, At: started, Stage: stage.Name, Phase: StageMeasuring})
	close(start)
	end := time.NewTimer(max(0, time.Until(started.Add(stage.Duration))))
	defer end.Stop()
	if len(stage.Directions) == 0 {
		for {
			select {
			case <-ctx.Done():
				return context.Cause(ctx)
			case <-end.C:
				normalEnd = true
				return nil
			case outcome := <-outcomes:
				if err := handle(outcome); err != nil {
					return err
				}
			}
		}
	}
	ticker := time.NewTicker(250 * time.Millisecond)
	defer ticker.Stop()
	sampled := make(chan sampledBoundary, 1)
	epoch := 0
	pending := false
	ending := false
	var sampleCancel context.CancelFunc
	var samples sync.WaitGroup
	defer func() {
		if sampleCancel != nil {
			sampleCancel()
		}
		samples.Wait()
	}()
	capture := func(final bool) {
		if pending {
			return
		}
		pending = true
		ownCtx, ownCancel := context.WithCancel(stageCtx)
		sampleCancel = ownCancel
		participants := c.active()
		ownEpoch := epoch
		samples.Go(func() {
			defer ownCancel()
			sampled <- sampledBoundary{c.capture(ownCtx, stage, participants), ownEpoch, final}
		})
	}
	lastBytes := map[string]byteLedger{}
	lastMovement := map[string]map[Direction]time.Time{}
	for _, server := range c.active() {
		id := server.prepared.Server.ID
		bytes := byteLedger{down: initial.down[id]}
		if initial.up[id] != nil {
			bytes.up = initial.up[id].Bytes
		}
		lastBytes[id] = bytes
		lastMovement[id] = map[Direction]time.Time{Down: started, Up: started}
	}
	reset := func() {
		epoch++
		if sampleCancel != nil {
			sampleCancel()
		}
		c.aggregate.begin(stage.Name, c.ids(), time.Since(c.started), "dropout")
		c.emitUnavailable(stage)
		if !pending {
			capture(ending)
		}
	}
	for {
		select {
		case <-ctx.Done():
			return context.Cause(ctx)
		case outcome := <-outcomes:
			before := len(c.ids())
			if err := handle(outcome); err != nil {
				return err
			}
			if len(c.ids()) != before {
				reset()
			}
		case <-end.C:
			ending = true
			capture(true)
		case <-ticker.C:
			if !ending {
				capture(false)
			}
		case sample := <-sampled:
			pending = false
			if sample.epoch != epoch {
				capture(ending)
				continue
			}
			window := c.aggregate.observe(sample.boundary)
			if window != nil {
				c.emitRates(stage, *window)
			} else {
				c.emitUnavailable(stage)
			}
			removed := false
			for _, server := range servers {
				if server.participant.removed {
					continue
				}
				id := server.participant.prepared.Server.ID
				bytes := byteLedger{down: sample.boundary.down[id]}
				if snapshot := sample.boundary.up[id]; snapshot != nil {
					bytes.up = snapshot.Bytes
				} else {
					bytes.up = sample.boundary.observedUp[id].maximum
				}
				for _, dir := range stage.Directions {
					advanced := dir == Down && bytes.down > lastBytes[id].down || dir == Up && bytes.up > lastBytes[id].up
					if advanced {
						lastMovement[id][dir] = time.Now()
					} else if !ending && server.participant.transport.targetTransport() == wire.TransportFetchStream && time.Since(lastMovement[id][dir]) >= busRedialWindow {
						c.failure(server, stage, string(dir), fmt.Errorf("%s stopped delivering bytes for %v", dir, busRedialWindow), time.Now())
						removed = true
						break
					}
				}
				lastBytes[id] = bytes
			}
			if len(c.ids()) == 0 {
				c.aggregate.begin(stage.Name, nil, time.Since(c.started), "dropout")
				return errNoSurvivors
			}
			if removed {
				reset()
				continue
			}
			c.publish("running")
			if sample.final {
				normalEnd = true
				return nil
			}
			if ending {
				capture(true)
			}
		}
	}
}

func (c *nativeCoordinator) capture(ctx context.Context, stage StagePlan, servers []*nativeParticipant) measurementBoundary {
	boundary := measurementBoundary{at: time.Since(c.started), down: map[string]uint64{}, up: map[string]*ReceiverSnapshot{}, observedUp: map[string]uploadLedger{}}
	for _, server := range servers {
		boundary.down[server.prepared.Server.ID] = server.transport.coordinated.download()
		id, bytes, _ := server.transport.coordinated.upload()
		if id != "" {
			boundary.observedUp[server.prepared.Server.ID] = uploadLedger{id, bytes}
		}
	}
	if stage.Name != "upload" && stage.Name != "bidirectional" {
		return boundary
	}
	ctx, cancel := context.WithTimeout(ctx, 1500*time.Millisecond)
	defer cancel()
	snapshots := make([]*ReceiverSnapshot, len(servers))
	var work sync.WaitGroup
	for i, server := range servers {
		work.Go(func() { snapshots[i], _ = server.transport.receiverCheckpoint(ctx, c.started) })
	}
	work.Wait()
	for i, server := range servers {
		boundary.up[server.prepared.Server.ID] = snapshots[i]
	}
	return boundary
}
func (c *nativeCoordinator) emitRates(stage StagePlan, window AggregateWindow) {
	for _, dir := range stage.Directions {
		rate := window.DownBytesPerSec
		if dir == Up {
			rate = window.UpBytesPerSec
		}
		if rate == nil {
			continue
		}
		var total uint64
		streams := 0
		for _, bytes := range c.aggregate.stageTotals[stage.Name] {
			if dir == Down {
				total += bytes.down
			} else {
				total += bytes.up
			}
		}
		for _, server := range c.active() {
			streams += server.transport.streams.of(dir)
		}
		c.emit(Event{Kind: EventThroughput, At: time.Now(), Stage: stage.Name, Direction: dir, Throughput: ThroughputSample{Stage: stage.Name, Direction: dir, BytesPerSec: *rate, TotalBytes: total, StreamCount: streams, ServerAuth: dir == Up}})
	}
}
func (c *nativeCoordinator) finishTransferStage(stage StagePlan, stageErr error) {
	for _, dir := range stage.Directions {
		result := c.aggregate.result(stage.Name, dir)
		result.Err = stageErr
		c.emit(Event{Kind: EventResult, At: time.Now(), Stage: stage.Name, Direction: dir, Result: new(result)})
		for _, server := range c.servers {
			id := server.prepared.Server.ID
			own := Result{Stage: stage.Name, Direction: dir, ServerAuth: dir == Up, Unavailable: true}
			total := c.aggregate.stageTotals[stage.Name][id]
			if dir == Down {
				own.TotalBytes = total.down
			} else {
				own.TotalBytes = total.up
			}
			for i := len(c.aggregate.intervals) - 1; i >= 0; i-- {
				interval := c.aggregate.intervals[i]
				if interval.Stage != stage.Name || interval.Window == nil {
					continue
				}
				components := interval.Window.Down
				if dir == Up {
					components = interval.Window.Up
				}
				found := false
				for _, component := range components {
					if component.ServerID == id {
						own.MeanBps = component.BytesPerSec
						own.Elapsed = component.Duration
						own.Unavailable = component.Duration < minimumSurvivorEvidence
						found = true
						break
					}
				}
				if found {
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

func (c *nativeCoordinator) emitUnavailable(stage StagePlan) {
	for _, dir := range stage.Directions {
		c.emit(Event{Kind: EventThroughput, At: time.Now(), Stage: stage.Name, Direction: dir, Throughput: ThroughputSample{Stage: stage.Name, Direction: dir, Unavailable: true}})
	}
}
