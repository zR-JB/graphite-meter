package goclient

import (
	"context"
	"time"

	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

// Inject concrete transports into the production coordinator. Stage readiness,
// warmup, measurement, result retention, and cleanup all use its ownership path.
func (r *runner) runTestStage(ctx context.Context, stage string, duration time.Duration) error {
	cfg := r.cfg
	cfg.Stages = StageSet{
		Latency:       stage == "latency",
		Download:      stage == "download",
		Upload:        stage == "upload",
		Bidirectional: stage == "bidirectional",
	}
	cfg.LatencyDuration, cfg.DownloadDuration, cfg.UploadDuration, cfg.BidirectionalDuration = duration, duration, duration, duration
	target := wire.ThroughputTarget{
		Origin:    cfg.BaseURL,
		Transport: r.targetTransport(),
		Routes:    r.routes(),
	}
	if r.target != nil {
		target = *r.target
	}
	prepared := PreparedServer{
		Server:     wire.ServerEntry{ID: "self", Name: "fixture", URL: cfg.BaseURL},
		Connection: &PreparedConnection{ThroughputTarget: target, LatencyTarget: r.latencyTarget},
		config:     cfg,
	}
	c := &nativeCoordinator{
		cfg:      cfg,
		prepared: &PreparedRun{Servers: []PreparedServer{prepared}, LatencyFocus: "self"},
		servers:  []*nativeParticipant{{prepared: prepared, transport: r}},
		streams:  map[string]map[string]streamCounts{stage: {"self": r.streams}},
		started:  time.Now(),
		emit:     r.emit,
	}
	return c.run(ctx)
}

func (r *runner) testTransferResult(ctx context.Context, stage string, duration time.Duration) (Result, error) {
	var result Result
	emit := r.emit
	r.emit = func(e Event) {
		if e.Kind == EventResult && e.Result.Direction != "" {
			result = *e.Result
		}
		emit(e)
	}
	defer func() { r.emit = emit }()
	err := r.runTestStage(ctx, stage, duration)
	return result, err
}
