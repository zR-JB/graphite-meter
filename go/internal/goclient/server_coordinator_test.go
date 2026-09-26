package goclient

import (
	"context"
	"encoding/json/v2"
	"errors"
	"io"
	"net/http"
	"net/http/httptest"
	"slices"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"github.com/zR-JB/graphite-meter/go/internal/endpoint"
	"github.com/zR-JB/graphite-meter/go/internal/route"
	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

type fixtureHTTP func(http.ResponseWriter, *http.Request) error

func (f fixtureHTTP) ServeHTTP(w http.ResponseWriter, r *http.Request) { _ = f(w, r) }

// fixtureRoutes mounts each fixture handler on its exact path.
type fixtureRoutes map[string]http.Handler

func (routes fixtureRoutes) RegisterHTTP(path string, h http.Handler) { routes[path] = h }

func (routes fixtureRoutes) Mount(_ context.Context, mux *http.ServeMux) {
	for path, h := range routes {
		mux.Handle(path, h)
	}
}

type pacedBody struct {
	io.ReadCloser
	ctx    context.Context
	failed *atomic.Bool
}

func (b pacedBody) Read(p []byte) (int, error) {
	if b.failed.Load() {
		return 0, errors.New("fixture disconnected")
	}
	timer := time.NewTimer(time.Millisecond)
	defer timer.Stop()
	select {
	case <-b.ctx.Done():
		return 0, b.ctx.Err()
	case <-timer.C:
	}
	return b.ReadCloser.Read(p[:min(len(p), 8192)])
}

type serverFixture struct {
	server               *httptest.Server
	catalog              wire.ServerCatalog
	failed               atomic.Bool
	checkpointFailed     atomic.Bool
	checkpointDelayNanos atomic.Int64
	active               atomic.Int32
	catalogReads         atomic.Int32
}

func coordinatedFixture(t *testing.T, name string) *serverFixture {
	t.Helper()
	f := &serverFixture{}
	upload := endpoint.NewUpload(nil, nil)
	registry := fixtureRoutes{}
	registry.RegisterHTTP(route.UploadSession, http.HandlerFunc(upload.ServeSession))
	registry.RegisterHTTP(route.UploadProgress, http.HandlerFunc(upload.ServeProgress))
	registry.RegisterHTTP(route.UploadCheckpoint, http.HandlerFunc(upload.ServeCheckpoint))
	registry.RegisterHTTP(route.Upload, upload)
	registry.RegisterHTTP(route.Preflight, fixtureHTTP(func(w http.ResponseWriter, r *http.Request) error {
		return json.MarshalWrite(w, wire.Preflight{
			Server:        wire.ServerInfo{Name: name},
			EngineVersion: "test",
			Generation:    name,
			Capabilities: wire.Capabilities{
				UploadCheckpoint: true,
				ThroughputTargets: []wire.ThroughputTarget{
					{Origin: ".", Transport: wire.TransportFetchStream, Protocol: "http1"},
				},
				LatencyTargets: []wire.LatencyTarget{{Origin: ".", Transport: wire.TransportWebSocket}},
			},
		})
	}))
	registry.RegisterHTTP(route.Servers, fixtureHTTP(func(w http.ResponseWriter, r *http.Request) error {
		f.catalogReads.Add(1)
		return json.MarshalWrite(w, f.catalog)
	}))
	registry.RegisterHTTP(route.Probe, http.HandlerFunc(writeProbe))
	registry.RegisterHTTP(route.Download, fixtureHTTP(func(w http.ResponseWriter, r *http.Request) error {
		block := make([]byte, 8192)
		timer := time.NewTicker(time.Millisecond)
		defer timer.Stop()
		for {
			select {
			case <-r.Context().Done():
				return nil
			case <-timer.C:
				if f.failed.Load() {
					return nil
				}
				if _, err := w.Write(block); err != nil {
					return nil
				}
				if err := http.NewResponseController(w).Flush(); err != nil {
					return nil
				}
			}
		}
	}))
	mux := http.NewServeMux()
	registry.Mount(t.Context(), mux)
	mux.Handle(route.Ping, echoPingHandler())
	f.server = httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		f.active.Add(1)
		defer f.active.Add(-1)
		if r.URL.Path == route.UploadCheckpoint {
			if delay := time.Duration(f.checkpointDelayNanos.Swap(0)); delay > 0 {
				timer := time.NewTimer(delay)
				defer timer.Stop()
				select {
				case <-r.Context().Done():
					return
				case <-timer.C:
				}
			}
		}
		if r.URL.Path == route.UploadCheckpoint && f.checkpointFailed.Load() {
			http.Error(w, "fixture checkpoint unavailable", http.StatusServiceUnavailable)
			return
		}
		if f.failed.Load() && (r.URL.Path == route.Download || r.URL.Path == route.Upload) {
			http.Error(w, "fixture dropout", http.StatusGone)
			return
		}
		if r.URL.Path == route.Upload {
			r.Body = pacedBody{r.Body, r.Context(), &f.failed}
		}
		mux.ServeHTTP(w, r)
	}))
	f.catalog = wire.SingletonCatalog()
	t.Cleanup(f.server.Close)
	return f
}
func prepareFixtureRun(t *testing.T, cfg Config, a, b *serverFixture) *PreparedRun {
	t.Helper()
	a.catalog = wire.ServerCatalog{
		DefaultSelection: []string{"self", "b"},
		Servers: []wire.ServerEntry{
			{ID: "self", URL: ".", Name: "A"},
			{ID: "b", URL: b.server.URL, Name: "B"},
		},
	}
	prepared, err := prepareRun(t.Context(), cfg, nil, nil)
	if err != nil {
		t.Fatal(err)
	}
	if b.catalogReads.Load() != 0 {
		t.Fatal("recursively imported the peer catalogue")
	}
	return prepared
}
func fixtureConfig(a *serverFixture) Config {
	cfg := DefaultConfig()
	cfg.BaseURL = a.server.URL
	cfg.Warmup = 10 * time.Millisecond
	cfg.LatencyDuration = time.Second
	cfg.DownloadDuration = 1400 * time.Millisecond
	cfg.UploadDuration = 1400 * time.Millisecond
	cfg.BidirectionalDuration = 1400 * time.Millisecond
	cfg.TransferStreams = TransferStreamPolicy{Forced: 1}
	cfg.LoadedLatency = false
	return cfg
}
func TestNativeCoordinatorRealBidirectional(t *testing.T) {
	t.Parallel()
	a, b := coordinatedFixture(t, "a"), coordinatedFixture(t, "b")
	cfg := fixtureConfig(a)
	cfg.Stages = StageSet{Bidirectional: true}
	cfg.LoadedLatency = true
	cfg.PingInterval = 25 * time.Millisecond
	prepared := prepareFixtureRun(t, cfg, a, b)
	var mu sync.Mutex
	var phases []Phase
	var results []Result
	var details *RunDetails
	err := runSelection(t.Context(), context.Background(), cfg, prepared, func(e Event) {
		mu.Lock()
		defer mu.Unlock()
		switch e.Kind {
		case EventStage:
			phases = append(phases, e.Phase)
		case EventResult:
			if e.ServerID == "" {
				results = append(results, *e.Result)
			}
		case EventServers:
			details = e.Servers
		}
	})
	if err != nil {
		t.Fatal(err)
	}
	if !slices.Equal(phases, []Phase{PhasePreparing, PhaseWarmup, PhaseMeasuring, PhaseFinished}) {
		t.Fatalf("more than one stage schedule: %v", phases)
	}
	if len(results) != 2 || details == nil || len(details.Participants) != 2 {
		t.Fatalf("results=%+v details=%+v", results, details)
	}
	for _, result := range results {
		if result.Unavailable || result.MeanBps <= 0 || result.TotalBytes == 0 {
			t.Fatalf("missing measured lane: %+v", result)
		}
	}
	for _, server := range details.Servers {
		measured := func(r Result) bool { return r.Direction == "" && r.Latency.Count > 0 }
		if !slices.ContainsFunc(server.Results, measured) {
			t.Fatalf("no independent loaded latency: %+v", server)
		}
	}
	window := details.Intervals[len(details.Intervals)-1].Window
	if len(window.Down) != 2 || len(window.Up) != 2 {
		t.Fatalf("component windows lost: %+v", window)
	}
	for _, component := range window.Up {
		if component.Clock != "receiver" || component.StartReceiver == nil || component.EndReceiver == nil {
			t.Fatalf("receiver evidence lost: %+v", component)
		}
	}
}

func TestNativeCoordinatorWaitsForCheckpointsBeforeStartingClientPopulations(t *testing.T) {
	t.Parallel()
	a, b := coordinatedFixture(t, "a"), coordinatedFixture(t, "b")
	a.checkpointDelayNanos.Store(int64(900 * time.Millisecond))
	cfg := fixtureConfig(a)
	cfg.Stages = StageSet{Bidirectional: true}
	cfg.BidirectionalDuration = time.Second
	cfg.LoadedLatency = true
	cfg.PingInterval = 25 * time.Millisecond
	prepared := prepareFixtureRun(t, cfg, a, b)
	var measuredAt, finishedAt time.Time
	var measureEventLag time.Duration
	var details *RunDetails
	var mu sync.Mutex
	err := runSelection(t.Context(), context.Background(), cfg, prepared, func(e Event) {
		mu.Lock()
		defer mu.Unlock()
		if e.Kind == EventStage && e.Phase == PhaseMeasuring {
			measuredAt = time.Now()
			measureEventLag = measuredAt.Sub(e.At)
		}
		if e.Kind == EventStage && e.Phase == PhaseFinished {
			finishedAt = time.Now()
		}
		if e.Servers != nil {
			details = e.Servers
		}
	})
	if err != nil {
		t.Fatal(err)
	}
	if measuredAt.IsZero() ||
		measureEventLag > 500*time.Millisecond ||
		finishedAt.Sub(measuredAt) < 900*time.Millisecond {
		t.Fatalf(
			"checkpoint wait consumed the client window: event lag=%v measured duration=%v",
			measureEventLag,
			finishedAt.Sub(measuredAt),
		)
	}
	if details == nil || len(details.Intervals) != 1 || details.Intervals[0].Window == nil {
		t.Fatalf("missing receiver window: %+v", details)
	}
	for _, component := range details.Intervals[0].Window.Up {
		if component.StartReceiver == nil || component.Clock != "receiver" {
			t.Fatalf("initial receiver snapshot missing: %+v", component)
		}
	}
	for _, server := range details.Servers {
		if !slices.ContainsFunc(server.Results, func(r Result) bool {
			return r.Direction == "" && r.Latency.Count > 0 && r.Latency.Elapsed >= 900*time.Millisecond
		}) {
			t.Fatalf("missing loaded latency population: %+v", server.Results)
		}
	}
}
func TestNativeCoordinatorDropout(t *testing.T) {
	t.Parallel()
	for _, scenario := range []struct {
		name      string
		at        time.Duration
		all       bool
		available bool
	}{{
		"survivor",
		300 * time.Millisecond,
		false,
		true,
	}, {"late", 950 * time.Millisecond, false, false}, {"all", 300 * time.Millisecond, true, false}} {
		t.Run(scenario.name, func(t *testing.T) {
			t.Parallel()
			a, b := coordinatedFixture(t, "a"), coordinatedFixture(t, "b")
			cfg := fixtureConfig(a)
			cfg.Stages = StageSet{Download: true}
			prepared := prepareFixtureRun(t, cfg, a, b)
			var details *RunDetails
			var result Result
			var timer *time.Timer
			err := runSelection(t.Context(), context.Background(), cfg, prepared, func(e Event) {
				if e.Kind == EventStage && e.Phase == PhaseMeasuring {
					timer = time.AfterFunc(scenario.at, func() {
						a.failed.Store(true)
						if scenario.all {
							b.failed.Store(true)
						}
					})
				}
				if e.Servers != nil {
					details = e.Servers
				}
				if e.Kind == EventResult && e.ServerID == "" {
					result = *e.Result
				}
			})
			if timer != nil {
				timer.Stop()
			}
			if scenario.all && !errors.Is(err, errNoSurvivors) || !scenario.all && err != nil {
				t.Fatalf("outcome=%v", err)
			}
			if result.Unavailable == scenario.available {
				t.Fatalf("survivor evidence selection=%+v", result)
			}
			if details == nil || len(details.Failures) == 0 || slices.Contains(details.Participants, "self") {
				t.Fatalf("failed participant retained: %+v", details)
			}
			if scenario.all && details.Outcome != OutcomeIncomplete {
				t.Fatalf("all failed outcome=%q", details.Outcome)
			}
		})
	}
}

func TestTransientCheckpointRefusalKeepsTheReceiverWindow(t *testing.T) {
	t.Parallel()
	a := coordinatedFixture(t, "a")
	cfg := fixtureConfig(a)
	cfg.Stages = StageSet{Upload: true}
	cfg.UploadDuration = time.Second
	prepared, err := prepareRun(t.Context(), cfg, nil, nil)
	if err != nil {
		t.Fatal(err)
	}
	refuse := func(after time.Duration) {
		time.AfterFunc(after, func() {
			a.checkpointFailed.Store(true)
			time.AfterFunc(300*time.Millisecond, func() { a.checkpointFailed.Store(false) })
		})
	}
	var upload Result
	err = runSelection(t.Context(), context.Background(), cfg, prepared, func(e Event) {
		switch {
		case e.Kind == EventStage && e.Phase == PhaseWarmup:
			refuse(0)
		case e.Kind == EventStage && e.Phase == PhaseMeasuring:
			refuse(cfg.UploadDuration - 100*time.Millisecond)
		case e.Kind == EventResult && e.Direction == Up:
			upload = *e.Result
		}
	})
	if err != nil || upload.Err != nil || upload.Unavailable || upload.MeanBps <= 0 {
		t.Fatalf("transient checkpoint refusal voided the stage: %v %+v", err, upload)
	}
}
