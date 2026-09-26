package goclient

import (
	"context"
	"time"

	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

// runDirect prepares one server without a catalogue and runs it through the production coordinator.
func runDirect(ctx context.Context, cfg Config, emit func(Event)) error {
	cfg = cfg.normalized()
	connection, err := prepare(ctx, cfg)
	if err != nil {
		emit(Event{Kind: EventDone, At: time.Now(), Err: err})
		return err
	}
	server := PreparedServer{Server: wire.ServerEntry{ID: "self", URL: cfg.BaseURL, Name: "fixture"}, Connection: connection, config: cfg}
	return runSelection(ctx, nil, cfg, &PreparedRun{Servers: []PreparedServer{server}, LatencyFocus: "self"}, emit)
}

// runTestStage injects concrete transports into the production coordinator. Stage readiness,
// warmup, measurement, result retention, and cleanup all use its ownership path.
func (r *runner) runTestStage(ctx context.Context, stage Stage, duration time.Duration) error {
	cfg := r.cfg
	cfg.Stages = StageSet{
		Latency:       stage == StageLatency,
		Download:      stage == StageDownload,
		Upload:        stage == StageUpload,
		Bidirectional: stage == StageBidirectional,
	}
	cfg.LatencyDuration, cfg.DownloadDuration, cfg.UploadDuration, cfg.BidirectionalDuration = duration, duration, duration, duration
	target := wire.ThroughputTarget{Origin: cfg.BaseURL, Transport: r.targetTransport(), Routes: r.routes()}
	if r.target != nil {
		target = *r.target
	}
	prepared := PreparedServer{
		Server:     wire.ServerEntry{ID: "self", Name: "fixture", URL: cfg.BaseURL},
		Connection: &PreparedConnection{ThroughputTarget: target, LatencyTarget: r.latencyTarget},
		config:     cfg,
	}
	c := &coordinator{
		cfg:      cfg,
		prepared: &PreparedRun{Servers: []PreparedServer{prepared}, LatencyFocus: "self"},
		servers:  []*participant{{prepared: prepared, transport: r}},
		started:  time.Now(),
		emit:     r.emit,
	}
	err := c.run(ctx)
	r.emit(Event{Kind: EventDone, At: time.Now(), Err: err, Servers: c.details(c.outcome(ctx, err))})
	return err
}

// testTransferResult runs one stage and returns its combined transfer result.
func (r *runner) testTransferResult(ctx context.Context, stage Stage, duration time.Duration) (Result, error) {
	var result Result
	emit := r.emit
	r.emit = func(e Event) {
		if e.Kind == EventResult {
			result = *e.Result
		}
		emit(e)
	}
	defer func() { r.emit = emit }()
	err := r.runTestStage(ctx, stage, duration)
	return result, err
}
