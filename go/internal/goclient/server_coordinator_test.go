package goclient

import (
	"context"
	"encoding/json/v2"
	"errors"
	"fmt"
	"net"
	"net/http"
	"net/http/httptest"
	"slices"
	"sync"
	"sync/atomic"
	"testing"
	"testing/synctest"
	"time"

	"github.com/zR-JB/graphite-meter/go/internal/endpoint"
	"github.com/zR-JB/graphite-meter/go/internal/route"
	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

type serverFixture struct {
	server               *httptest.Server
	catalog              wire.ServerCatalog
	failed, revoked      atomic.Bool
	silent               atomic.Bool
	checkpointFailed     atomic.Bool
	checkpointRefusals   atomic.Int32
	checkpointDelayNanos atomic.Int64
	handlers, conns      sync.WaitGroup
	catalogReads         atomic.Int32
	dropLatency          func()
	upload               atomic.Pointer[endpoint.Upload]
}

// restart replaces the receiver store and drops every connection, as a restarted server does.
func (f *serverFixture) restart() {
	f.upload.Store(endpoint.NewUpload(nil, nil))
	f.server.CloseClientConnections()
}

func coordinatedFixture(t *testing.T, name string) *serverFixture {
	t.Helper()
	f := &serverFixture{}
	f.upload.Store(endpoint.NewUpload(nil, nil))
	mux := http.NewServeMux()
	receiver := func(serve func(*endpoint.Upload, http.ResponseWriter, *http.Request)) http.HandlerFunc {
		return func(w http.ResponseWriter, r *http.Request) { serve(f.upload.Load(), w, r) }
	}
	mux.HandleFunc(route.UploadSession, receiver((*endpoint.Upload).ServeSession))
	mux.HandleFunc(route.UploadProgress, receiver((*endpoint.Upload).ServeProgress))
	mux.HandleFunc(route.UploadCheckpoint, receiver((*endpoint.Upload).ServeCheckpoint))
	mux.HandleFunc(route.Upload, receiver(func(u *endpoint.Upload, w http.ResponseWriter, r *http.Request) {
		u.Handler(wire.IdleBound).ServeHTTP(w, r)
	}))
	mux.HandleFunc(route.Preflight, func(w http.ResponseWriter, r *http.Request) {
		_ = json.MarshalWrite(w, wire.Preflight{
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
	})
	mux.HandleFunc(route.Servers, func(w http.ResponseWriter, r *http.Request) {
		f.catalogReads.Add(1)
		_ = json.MarshalWrite(w, f.catalog)
	})
	mux.HandleFunc(route.Probe, writeProbe)
	mux.HandleFunc(route.Download, func(w http.ResponseWriter, r *http.Request) {
		block := make([]byte, 8192)
		for range time.Tick(time.Millisecond) {
			if r.Context().Err() != nil || f.failed.Load() {
				return
			}
			if _, err := w.Write(block); err != nil || http.NewResponseController(w).Flush() != nil {
				return
			}
		}
	})
	latency, dropLatency := context.WithCancel(t.Context())
	mux.Handle(route.Ping, http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		ctx, cancel := context.WithCancel(r.Context())
		defer cancel()
		defer context.AfterFunc(latency, cancel)()
		pingHandler(func(uint32) bool { return !f.silent.Load() }, 0).ServeHTTP(w, r.WithContext(ctx))
	}))
	f.server = httptest.NewUnstartedServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		f.handlers.Add(1)
		defer f.handlers.Done()
		if f.revoked.Load() {
			w.Header().Set("Graphite-Meter-Auth", "required")
			w.WriteHeader(http.StatusForbidden)
			return
		}
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
		refused := func() bool { return f.checkpointFailed.Load() || f.checkpointRefusals.Add(-1) >= 0 }
		if r.URL.Path == route.UploadCheckpoint && refused() {
			http.Error(w, "fixture checkpoint unavailable", http.StatusServiceUnavailable)
			return
		}
		if f.failed.Load() && (r.URL.Path == route.Download || r.URL.Path == route.Upload) {
			http.Error(w, "fixture dropout", http.StatusGone)
			return
		}
		if r.URL.Path == route.Upload {
			r.Body = pacedBody{r.Body, r.Context(), f.failed.Load}
		}
		mux.ServeHTTP(w, r)
	}))
	f.server.Config.ConnState = func(_ net.Conn, state http.ConnState) {
		switch state {
		case http.StateNew:
			f.conns.Add(1)
		case http.StateClosed, http.StateHijacked:
			f.conns.Done()
		}
	}
	f.server.Start()
	f.dropLatency = func() {
		dropLatency()
		_ = f.server.Listener.Close()
	}
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
	a.checkpointDelayNanos.Store(int64(900 * time.Millisecond))
	cfg := fixtureConfig(a)
	cfg.Stages = StageSet{Bidirectional: true}
	cfg.LoadedLatency = true
	cfg.PingInterval, cfg.LoadedPingInterval = 25*time.Millisecond, 25*time.Millisecond
	prepared := prepareFixtureRun(t, cfg, a, b)
	var log eventLog
	var measuredAt, finishedAt time.Time
	var measureEventLag time.Duration
	err := runSelected(t.Context(), cfg, prepared, func(e Event) {
		if e.Kind == EventStage && e.Phase == PhaseMeasuring {
			measuredAt = time.Now()
			measureEventLag = measuredAt.Sub(e.At)
		}
		if e.Kind == EventStage && e.Phase == PhaseFinished {
			finishedAt = time.Now()
		}
		log.emit(e)
	})
	if err != nil {
		t.Fatal(err)
	}
	schedule := []Phase{PhasePreparing, PhaseWarmup, PhaseMeasuring, PhaseFinished}
	if phases := log.phases(); !slices.Equal(phases, schedule) {
		t.Fatalf("more than one stage schedule: %v", phases)
	}
	if measureEventLag > 500*time.Millisecond || finishedAt.Sub(measuredAt) < 900*time.Millisecond {
		t.Fatalf("the receiver checkpoint wait consumed the client window: event lag=%v measured duration=%v",
			measureEventLag, finishedAt.Sub(measuredAt))
	}
	results, details := log.results(), log.details()
	if len(results) != 2 || details == nil || len(details.Participants) != 2 || len(details.Intervals) != 1 {
		t.Fatalf("results=%+v details=%+v", results, details)
	}
	for _, result := range results {
		if result.Unavailable || result.MeanBps <= 0 || result.TotalBytes == 0 {
			t.Fatalf("missing measured lane: %+v", result)
		}
	}
	for _, server := range details.Servers {
		measured := func(r Result) bool {
			return r.Direction == "" && r.Latency.Count > 0 && r.Latency.Elapsed >= 900*time.Millisecond
		}
		if !slices.ContainsFunc(server.Results, measured) {
			t.Fatalf("no independent loaded latency over the client window: %+v", server)
		}
		for _, own := range server.Results {
			if own.Direction != "" && (own.PeakBps <= 0 || own.Samples == 0) {
				t.Fatalf("per-server result lost its peak or samples: %+v", own)
			}
		}
	}
	if window := details.Intervals[0].Window; window == nil || len(window.Down) != 2 || len(window.Up) != 2 {
		t.Fatalf("component windows lost: %+v", window)
	}
}

func TestNativeCoordinatorDropout(t *testing.T) {
	t.Parallel()
	for _, scenario := range []struct {
		name      string
		after     int
		all       bool
		available bool
	}{{"survivor", 1, false, true}, {"late", 4, false, true}, {"all", 1, true, false}} {
		t.Run(scenario.name, func(t *testing.T) {
			t.Parallel()
			a, b := coordinatedFixture(t, "a"), coordinatedFixture(t, "b")
			cfg := fixtureConfig(a)
			cfg.Stages = StageSet{Download: true}
			prepared := prepareFixtureRun(t, cfg, a, b)
			var log eventLog
			samples := 0
			err := runSelected(t.Context(), cfg, prepared, func(e Event) {
				if e.Kind == EventThroughput && !e.Throughput.Unavailable {
					if samples++; samples == scenario.after {
						a.failed.Store(true)
						b.failed.Store(scenario.all)
					}
				}
				log.emit(e)
			})
			result, details := log.results()[0], log.details()
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
			left := map[string]error{}
			for _, f := range details.Failures {
				left[f.ServerID] = f.Err
			}
			for _, server := range details.Servers {
				own, cause := server.Results[0], left[server.Server.ID]
				if cause != nil && own.Err != cause || cause == nil && own.Unavailable != result.Unavailable {
					t.Errorf("%s kept a result its interval does not support: %+v", server.Server.ID, own)
				}
			}
		})
	}
}

func TestASoleServerRetriesAtItsNextStage(t *testing.T) {
	t.Parallel()
	a := coordinatedFixture(t, "a")
	cfg := fixtureConfig(a)
	cfg.Stages = StageSet{Download: true, Upload: true}
	cfg.DownloadDuration, cfg.UploadDuration = time.Second, time.Second
	samples := 0
	var log eventLog
	err := Run(t.Context(), cfg, func(e Event) {
		switch {
		case e.Kind == EventThroughput && e.Stage == StageDownload:
			if samples++; samples == 2 {
				a.failed.Store(true)
			}
		case e.Kind == EventStage && e.Stage == StageDownload && e.Phase == PhaseFinished:
			a.failed.Store(false)
		}
		log.emit(e)
	})
	results, details := log.results(), log.details()
	if err != nil || details == nil || details.Outcome != OutcomeIncomplete || len(details.Failures) != 1 ||
		len(results) != 2 || !results[0].Unavailable || results[1].Unavailable {
		t.Fatalf("a sole server's failed stage ended the run: %v %+v %+v", err, details, results)
	}
}

func TestASoleServerLosesAStageItCannotMeasure(t *testing.T) {
	t.Parallel()
	for _, revoked := range []bool{false, true} {
		t.Run(fmt.Sprint("revoked ", revoked), func(t *testing.T) {
			t.Parallel()
			a := coordinatedFixture(t, "a")
			fault := &a.checkpointFailed
			if revoked {
				fault = &a.revoked
			}
			cfg := fixtureConfig(a)
			cfg.Stages = StageSet{Download: true, Upload: true}
			var log eventLog
			err := Run(t.Context(), cfg, func(e Event) {
				if e.Kind == EventStage && e.Stage == StageDownload && e.Phase == PhaseFinished {
					fault.Store(true)
				}
				log.emit(e)
			})
			results, details := log.results(), log.details()
			if details == nil || details.Outcome != OutcomeIncomplete || len(results) == 0 || results[0].Unavailable {
				t.Fatalf("%v: %+v %+v", err, details, results)
			}
			switch _, signIn := errors.AsType[*AuthRequiredError](err); {
			case revoked && !signIn:
				t.Fatalf("a revoked grant ended with %v, want the sign-in prompt", err)
			case !revoked && (err != nil || len(results) != 2 || !results[1].Unavailable || results[1].Err == nil):
				t.Fatalf("an unprepared upload = %v, %+v; want its stage failed with a reason", err, results)
			}
		})
	}
}

func TestARestartedServerGetsOneReplacementReceiver(t *testing.T) {
	t.Parallel()
	for _, restarts := range []int32{1, 2} {
		t.Run(fmt.Sprint(restarts, " restarts"), func(t *testing.T) {
			t.Parallel()
			a := coordinatedFixture(t, "a")
			cfg := fixtureConfig(a)
			cfg.Stages, cfg.UploadDuration = StageSet{Upload: true}, 3*time.Second
			var log eventLog
			var restarted atomic.Int32
			err := Run(t.Context(), cfg, func(e Event) {
				log.emit(e)
				if e.Kind == EventThroughput && !e.Throughput.Unavailable && restarted.Load() < restarts {
					restarted.Add(1)
					a.restart()
				}
			})
			details := log.details()
			if err != nil || details == nil || restarted.Load() != restarts {
				t.Fatalf("run = %v after %d restarts: %+v", err, restarted.Load(), details)
			}
			resumed := slices.IndexFunc(details.Intervals, func(i AggregationInterval) bool {
				return i.Reason == ReasonEvidenceResumed
			})
			if resumed != 1 || details.Intervals[0].Complete {
				t.Fatalf("the replaced receiver did not resume evidence: %+v", details.Intervals)
			}
			if restarts == 1 && (details.Outcome != OutcomeComplete || log.results()[0].Unavailable) {
				t.Fatalf("one restart lost the upload: %+v %+v", details, log.results())
			}
			if restarts == 2 && (len(details.Failures) != 1 || details.Failures[0].Reason != FailureConnectionLost) {
				t.Fatalf("a second unknown id = %+v, want the server lost as connection-lost", details.Failures)
			}
		})
	}
}

func TestTheLatencyResultFollowsTheFocusServer(t *testing.T) {
	t.Parallel()
	for _, silentFocus := range []bool{false, true} {
		t.Run(fmt.Sprint("silent focus ", silentFocus), func(t *testing.T) {
			t.Parallel()
			a, b := coordinatedFixture(t, "a"), coordinatedFixture(t, "b")
			cfg := fixtureConfig(a)
			cfg.Stages = StageSet{Latency: true}
			prepared := prepareFixtureRun(t, cfg, a, b)
			prepared.LatencyFocus = "b"
			silent, id, want := a, "self", OutcomePartial
			if silentFocus {
				silent, id, want = b, "b", OutcomeIncomplete
			}
			silent.silent.Store(true)
			var log eventLog
			err := runSelected(t.Context(), cfg, prepared, log.emit)
			details := log.details()
			if err != nil || details == nil || details.Outcome != want || len(details.Failures) != 1 {
				t.Fatalf("%v: %+v", err, details)
			}
			if f := details.Failures[0]; f.Reason != FailureInsufficientEvidence || f.Scope != ScopeLatency ||
				f.ServerID != id {
				t.Fatalf("a silent population = %+v, want its reason recorded", f)
			}
		})
	}
}

func TestALatencyStageLossLeavesTheLaterStages(t *testing.T) {
	t.Parallel()
	a, b := coordinatedFixture(t, "a"), coordinatedFixture(t, "b")
	cfg := fixtureConfig(a)
	cfg.Stages = StageSet{Latency: true, Download: true}
	prepared := prepareFixtureRun(t, cfg, a, b)
	b.dropLatency()
	var log eventLog
	err := runSelected(t.Context(), cfg, prepared, log.emit)
	details := log.details()
	if err != nil || details == nil || details.Outcome != OutcomePartial ||
		!slices.Equal(details.Participants, []string{"self"}) || len(details.Failures) != 1 {
		t.Fatalf("the lost server stayed in the run: %v %+v", err, details)
	}
	lost := details.Failures[0]
	if lost.ServerID != "b" || lost.Stage != StageLatency || lost.Reason != FailureConnectionLost {
		t.Fatalf("failure = %+v", lost)
	}
	for _, own := range details.Servers[1].Results {
		if own.Direction == Down && (!own.Unavailable || own.Err.Error() != "left the test during latency") {
			t.Fatalf("the lost server's download does not name its cause: %+v", own)
		}
	}
	if download := log.results()[0]; download.Unavailable {
		t.Fatalf("the survivor measured no download: %+v", download)
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
	var log eventLog
	samples := 0
	err = runSelected(t.Context(), cfg, prepared, func(e Event) {
		switch {
		case e.Kind == EventStage && e.Phase == PhaseWarmup:
			a.checkpointRefusals.Store(3)
		case e.Kind == EventThroughput && e.Direction == Up:
			if samples++; samples == 3 {
				a.checkpointRefusals.Store(3)
			}
		}
		log.emit(e)
	})
	if upload := log.results()[0]; err != nil || upload.Err != nil || upload.Unavailable || upload.MeanBps <= 0 {
		t.Fatalf("transient checkpoint refusal voided the stage: %v %+v", err, upload)
	}
}

func TestARemovedServersLatencyKeepsItsCause(t *testing.T) {
	t.Parallel()
	synctest.Test(t, func(t *testing.T) {
		r := pipedRunner(t, pingHandler(answerAll, time.Millisecond))
		r.cfg.PingInterval = 20 * time.Millisecond
		ctx, remove := context.WithCancelCause(t.Context())
		removed := errors.New("server removed")
		time.AfterFunc(100*time.Millisecond, func() { remove(removed) })
		stats, err := r.measureNow(ctx, false, time.Second)
		s := &stageServer{participant: &participant{transport: r}}
		result := Result{Stage: StageDownload, Latency: stats, Err: err}
		(&coordinator{}).retainLatency(resourceOutcome{server: s, role: roleLatency, result: result, err: err}, true)
		if stats.Count == 0 || len(s.results) != 1 || !errors.Is(s.results[0].Err, removed) {
			t.Fatalf("a removed server's population was saved clean: %+v", s.results)
		}
	})
}

func TestLoadedLatencyKeepsTheIdleRTT(t *testing.T) {
	t.Parallel()
	c := &coordinator{}
	s := &stageServer{participant: &participant{transport: &runner{idleRTT: 5 * time.Millisecond}}}
	for _, step := range []struct {
		stage     Stage
		p50, want time.Duration
	}{
		{StageDownload, 40 * time.Millisecond, 5 * time.Millisecond},
		{StageLatency, 7 * time.Millisecond, 7 * time.Millisecond},
		{StageUpload, 40 * time.Millisecond, 7 * time.Millisecond},
	} {
		result := Result{Stage: step.stage, Latency: LatencyStats{Count: 1, P50: step.p50}}
		c.retainLatency(resourceOutcome{server: s, role: roleLatency, result: result}, true)
		if s.transport.idleRTT != step.want {
			t.Fatalf("after %s latency the idle RTT is %v, want %v", step.stage, s.transport.idleRTT, step.want)
		}
	}
}
