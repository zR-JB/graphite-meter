package goclient

import (
	"context"
	"encoding/json/v2"
	"net/http"
	"net/http/httptest"
	"time"

	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

func runDirect(ctx context.Context, cfg Config, emit func(Event)) error {
	cfg = cfg.normalized()
	connection, err := prepare(ctx, cfg)
	if err != nil {
		emit(Event{Kind: EventDone, At: time.Now(), Err: err})
		return err
	}
	server := PreparedServer{
		Server:     wire.ServerEntry{ID: "self", URL: cfg.BaseURL, Name: "fixture"},
		Connection: connection,
		config:     cfg,
	}
	prepared := &PreparedRun{Servers: []PreparedServer{server}, LatencyFocus: "self"}
	return runSelection(ctx, context.WithoutCancel(ctx), cfg, prepared, emit)
}

func (r *runner) runTestStage(ctx context.Context, stage Stage, duration time.Duration) error {
	cfg := r.cfg
	cfg.Stages = StageSet{
		Latency:       stage == StageLatency,
		Download:      stage == StageDownload,
		Upload:        stage == StageUpload,
		Bidirectional: stage == StageBidirectional,
	}
	for _, d := range []*time.Duration{
		&cfg.LatencyDuration,
		&cfg.DownloadDuration,
		&cfg.UploadDuration,
		&cfg.BidirectionalDuration,
	} {
		*d = duration
	}
	if r.target == nil {
		r.target = fetchTarget(cfg.BaseURL)
	}
	if r.teardown == nil {
		r.teardown = context.WithoutCancel(ctx)
	}
	prepared := PreparedServer{
		Server:     wire.ServerEntry{ID: "self", Name: "fixture", URL: cfg.BaseURL},
		Connection: &PreparedConnection{ThroughputTarget: *r.target, LatencyTarget: r.latencyTarget},
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

func testRunner(srv *httptest.Server) *runner {
	return &runner{
		cfg:     Config{BaseURL: srv.URL}.normalized(),
		http:    srv.Client(),
		target:  fetchTarget(srv.URL),
		streams: streamCounts{down: 1, up: 1},
		emit:    func(Event) {},
	}
}

func fetchTarget(origin string) *wire.ThroughputTarget {
	target := testTransfer(origin, origin, "http1", false)
	return &target
}

func writeProbe(w http.ResponseWriter, _ *http.Request) {
	_ = json.MarshalWrite(w, wire.Probe{
		ClientIP:           "127.0.0.1",
		ClientIPVersion:    4,
		ClientIPSource:     "socket",
		ProtocolNegotiated: "http/1.1",
	})
}
