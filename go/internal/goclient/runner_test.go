package goclient

import (
	"context"
	"encoding/json/v2"
	"errors"
	"fmt"
	"net/http"
	"net/http/httptest"
	"net/http/httputil"
	"net/url"
	"slices"
	"strconv"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"testing/synctest"
	"time"

	"github.com/coder/websocket"
	"github.com/quic-go/quic-go/http3"
	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

const captureWindow = 2 * time.Second

/* ---- pure helper functions ---- */

func TestHTTP3ClientStartsAtMinimumPacketSize(t *testing.T) {
	hc, closeClient := protocolClient(DefaultConfig(), "http3", func() *http.Transport { return &http.Transport{} })
	defer closeClient()
	wrapped, ok := hc.Transport.(authTransport)
	if !ok {
		t.Fatal("HTTP/3 client has no authentication boundary")
	}
	tr, ok := wrapped.base.(*http3.Transport)
	if !ok || tr.QUICConfig == nil {
		t.Fatal("HTTP/3 client has no QUIC configuration")
	}
	if tr.QUICConfig.InitialPacketSize != 1200 {
		t.Fatalf("initial packet size = %d, want 1200", tr.QUICConfig.InitialPacketSize)
	}
}

func TestAdaptiveWarmup(t *testing.T) {
	const base = 500 * time.Millisecond
	cases := []struct {
		name string
		rtt  time.Duration
		want time.Duration
	}{
		{"no rtt measured yet uses the floor", 0, base},
		{"negative rtt uses the floor", -1, base},
		{"small rtt below the floor still uses the floor", 10 * time.Millisecond, base},
		{"mid rtt scales to 10 RTTs", 100 * time.Millisecond, time.Second},
		{"large rtt is capped at the ceiling", time.Second, 4 * time.Second},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			if got := adaptiveWarmup(base, c.rtt); got != c.want {
				t.Errorf("adaptiveWarmup(%v, %v) = %v, want %v", base, c.rtt, got, c.want)
			}
		})
	}
}

func TestLaneStaggerStep(t *testing.T) {
	cases := []struct {
		name    string
		streams int
		warmup  time.Duration
		idleRTT time.Duration
		want    time.Duration
	}{
		{"single stream never staggers", 1, time.Second, 0, 0},
		{"zero streams (defensive) never staggers", 0, time.Second, 0, 0},
		{"short warmup divides evenly", 3, 200 * time.Millisecond, 0, 50 * time.Millisecond},
		{"long warmup is capped at laneStagger", 3, 4 * time.Second, 0, laneStagger},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			r := &runner{cfg: Config{Warmup: c.warmup}, idleRTT: c.idleRTT}
			if got := r.laneStaggerStep(c.streams); got != c.want {
				t.Errorf("laneStaggerStep() = %v, want %v", got, c.want)
			}
		})
	}
}

func TestStaggerSleep(t *testing.T) {
	t.Run("lane 0 returns immediately", func(t *testing.T) {
		start := time.Now()
		if !staggerSleep(t.Context(), 0, time.Hour) {
			t.Fatal("want true for lane 0")
		}
		if time.Since(start) > 100*time.Millisecond {
			t.Error("lane 0 should not wait regardless of step")
		}
	})

	t.Run("zero step returns immediately", func(t *testing.T) {
		if !staggerSleep(t.Context(), 5, 0) {
			t.Fatal("want true for a zero step")
		}
	})

	t.Run("waits out the delay then succeeds", func(t *testing.T) {
		start := time.Now()
		if !staggerSleep(t.Context(), 2, 20*time.Millisecond) {
			t.Fatal("want true once the delay elapses")
		}
		if elapsed := time.Since(start); elapsed < 30*time.Millisecond {
			t.Errorf("returned after %v, want at least ~40ms (lane 2 * step 20ms)", elapsed)
		}
	})

	t.Run("cancelled context aborts the wait", func(t *testing.T) {
		ctx, cancel := context.WithCancel(t.Context())
		cancel()
		if staggerSleep(ctx, 3, time.Hour) {
			t.Fatal("want false once the context is already cancelled")
		}
	})
}

func TestRunPreparationFailureHasOneTerminalOutcome(t *testing.T) {
	for _, cancelled := range []bool{false, true} {
		t.Run(fmt.Sprint(cancelled), func(t *testing.T) {
			ctx, cancel := context.WithCancel(t.Context())
			defer cancel()
			if cancelled {
				cancel()
			}
			var events []Event
			err := Run(ctx, Config{BaseURL: ":invalid"}, func(e Event) { events = append(events, e) })
			if err == nil || len(events) != 1 || events[0].Kind != EventDone || events[0].Err != err {
				t.Fatalf("Run = %v, events = %+v; want one matching terminal error", err, events)
			}
		})
	}
}

func TestRunnerEndpoint(t *testing.T) {
	r := &runner{cfg: Config{BaseURL: "http://example.test/base/"}}

	t.Run("empty path is rejected", func(t *testing.T) {
		if _, err := r.endpoint(""); err == nil {
			t.Fatal("want an error for an empty endpoint path")
		}
	})

	t.Run("joins base and path", func(t *testing.T) {
		got, err := r.endpoint("/download")
		if err != nil {
			t.Fatalf("endpoint: %v", err)
		}
		if want := "http://example.test/base/download"; got != want {
			t.Errorf("endpoint(/download) = %q, want %q", got, want)
		}
	})
}

func testStageGate(start chan struct{}) *stageGate {
	return &stageGate{start: start, ready: make(chan struct{}, 1), cancel: func(error) {}}
}

func TestStageGateWaitsForEveryParticipantBeforeWarmup(t *testing.T) {
	for _, warmup := range []time.Duration{0, 30 * time.Millisecond} {
		t.Run(warmup.String(), func(t *testing.T) {
			synctest.Test(t, func(t *testing.T) {
				var phases []StagePhase
				r := &runner{cfg: Config{Warmup: warmup}, emit: func(e Event) { phases = append(phases, e.Phase) }}
				gate := r.newStageGate(t.Context(), "bidirectional", 2)
				defer gate.stop()
				gate.ready <- struct{}{}
				time.Sleep(200 * time.Millisecond)
				synctest.Wait()
				if len(phases) != 1 || phases[0] != StagePreparing {
					t.Fatalf("one direction still preparing: phases=%v", phases)
				}
				gate.ready <- struct{}{}
				synctest.Wait()
				if warmup > 0 {
					select {
					case <-gate.start:
						t.Fatal("warmup skipped after readiness")
					default:
					}
					time.Sleep(warmup)
					synctest.Wait()
				}
				select {
				case <-gate.start:
				default:
					t.Fatal("measured window not opened")
				}
				want := []StagePhase{StagePreparing, StageMeasuring}
				if warmup > 0 {
					want = []StagePhase{StagePreparing, StageWarmup, StageMeasuring}
				}
				if !slices.Equal(phases, want) {
					t.Fatalf("phases=%v, want %v", phases, want)
				}
			})
		})
	}
}

func TestStageGateSetupTimeoutAndCancellationCannotEmitMeasuring(t *testing.T) {
	for _, cancelEarly := range []bool{false, true} {
		t.Run(fmt.Sprint(cancelEarly), func(t *testing.T) {
			synctest.Test(t, func(t *testing.T) {
				var phases []StagePhase
				r := &runner{emit: func(e Event) { phases = append(phases, e.Phase) }}
				ctx, cancel := context.WithCancel(t.Context())
				defer cancel()
				gate := r.newStageGate(ctx, "download", 1)
				defer gate.stop()
				if cancelEarly {
					cancel()
				} else {
					time.Sleep(stageReadyTimeout)
				}
				synctest.Wait()
				if gate.ctx.Err() == nil || !slices.Equal(phases, []StagePhase{StagePreparing}) {
					t.Fatalf("unready stage: cause=%v phases=%v", context.Cause(gate.ctx), phases)
				}
				gate.ready <- struct{}{}
				synctest.Wait()
				select {
				case <-gate.start:
					t.Fatal("failed readiness opened the window")
				default:
				}
			})
		})
	}
}

func TestRunLatencyStageCapturesIdleRTT(t *testing.T) {
	srv := newEchoPingServer(t)
	defer srv.Close()

	cfg := Config{BaseURL: srv.URL, PingInterval: 20 * time.Millisecond}.normalized()
	r := &runner{cfg: cfg, http: srv.Client(), emit: func(Event) {}}
	attachTestLatencyTarget(r, srv.URL)

	ctx, cancel := context.WithTimeout(t.Context(), 10*time.Second)
	defer cancel()
	if err := r.runLatencyStage(ctx, "latency", false, captureWindow); err != nil {
		t.Fatalf("runLatencyStage: %v", err)
	}
	if r.idleRTT <= 0 {
		t.Error("idleRTT was not captured from the unloaded latency stage")
	}
}

/* ---- Run() stage-orchestration end-to-end ---- */

func mountDiscovery(mux *http.ServeMux) {
	mux.HandleFunc("/preflight", func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		origin := "http://" + r.Host
		_ = json.MarshalWrite(w, wire.Preflight{Server: wire.ServerInfo{Name: "test"}, EngineVersion: "test", Generation: "test", Capabilities: wire.Capabilities{
			ThroughputTargets: []wire.ThroughputTarget{testTransfer("http1-clear", origin, "http1", false)},
			LatencyTargets:    []wire.LatencyTarget{testChannel("ws-http1-clear", origin, false)},
		}})
	})
	mux.HandleFunc("/probe", func(w http.ResponseWriter, r *http.Request) {
		_ = json.MarshalWrite(w, wire.Probe{ClientIP: "127.0.0.1", ClientIPVersion: 4, ClientIPSource: "socket", ProtocolNegotiated: "http/1.1"})
	})
}

func newDownloadOnlyServer(t *testing.T) *httptest.Server {
	t.Helper()
	mux := http.NewServeMux()
	mountDiscovery(mux)
	mux.HandleFunc("/download", func(w http.ResponseWriter, r *http.Request) {
		n, err := strconv.ParseInt(r.URL.Query().Get("bytes"), 10, 64)
		if err != nil || n <= 0 {
			n = 64 * 1024
		}
		w.Header().Set("Content-Type", "application/octet-stream")
		_, _ = w.Write(make([]byte, n))
	})
	return httptest.NewServer(mux)
}

func TestRunDownloadStageEndToEnd(t *testing.T) {
	srv := newDownloadOnlyServer(t)
	defer srv.Close()

	cfg := Config{
		BaseURL:                srv.URL,
		Stages:                 StageSet{Download: true},
		Warmup:                 0,
		DownloadDuration:       captureWindow,
		TransferStreams:        TransferStreamPolicy{Forced: 1},
		DownloadBytesPerStream: 128 * 1024,
	}

	var mu sync.Mutex
	var events []Event
	ctx, cancel := context.WithTimeout(t.Context(), 10*time.Second)
	defer cancel()
	if err := Run(ctx, cfg, func(e Event) { mu.Lock(); events = append(events, e); mu.Unlock() }); err != nil {
		t.Fatalf("Run: %v", err)
	}

	mu.Lock()
	defer mu.Unlock()
	var result *Result
	var terminals int
	for _, e := range events {
		if e.Kind == EventResult && e.Stage == "download" {
			result = e.Result
		}
		if e.Kind == EventDone {
			terminals++
		}
	}
	if result == nil {
		t.Fatal("no download Result event emitted")
	}
	if result.TotalBytes == 0 {
		t.Error("download result reports zero bytes")
	}
	if result.Samples == 0 || result.MeanBps <= 0 {
		t.Fatalf("download result lacks throughput samples or a positive window rate: %+v", result)
	}
	if terminals != 1 || events[len(events)-1].Kind != EventDone || events[len(events)-2].Phase != StageFinished {
		t.Fatalf("want one terminal outcome after the finished stage, got %+v", events)
	}
}

func TestRunAcceptsProxyProtocolBoundary(t *testing.T) {
	var origin string
	var probeRequestProtocol string
	mux := http.NewServeMux()
	mux.HandleFunc("/preflight", func(w http.ResponseWriter, r *http.Request) {
		_ = json.MarshalWrite(w, wire.Preflight{
			Generation: "test", Server: wire.ServerInfo{Name: "proxy"},
			Capabilities: wire.Capabilities{ThroughputTargets: []wire.ThroughputTarget{
				testTransfer("http2", origin, "http2", true),
			}},
		})
	})
	mux.HandleFunc("/probe", func(w http.ResponseWriter, r *http.Request) {
		probeRequestProtocol = r.Proto
		_ = json.MarshalWrite(w, wire.Probe{
			ClientIP: "127.0.0.1", ClientIPVersion: 4, ClientIPSource: "socket",
			ProtocolNegotiated: "http/1.1", // proxy-to-Go evidence
		})
	})
	mux.HandleFunc("/download", func(w http.ResponseWriter, r *http.Request) {
		_, _ = w.Write(make([]byte, 64*1024))
	})
	srv := httptest.NewUnstartedServer(mux)
	srv.EnableHTTP2 = true
	srv.StartTLS()
	defer srv.Close()
	origin = srv.URL

	cfg := Config{
		BaseURL: origin, ThroughputTarget: "auto", Stages: StageSet{Download: true},
		DownloadDuration: 100 * time.Millisecond, InsecureSkipTLSVerify: true,
		TransferStreams: TransferStreamPolicy{Forced: 1}, DownloadBytesPerStream: 64 * 1024,
	}
	ctx, cancel := context.WithTimeout(t.Context(), 5*time.Second)
	defer cancel()
	if err := Run(ctx, cfg, func(Event) {}); err != nil {
		t.Fatalf("Run through H2 proxy with H1 downstream evidence: %v", err)
	}
	if probeRequestProtocol != "HTTP/2.0" {
		t.Fatalf("client-to-proxy probe used %q, want HTTP/2.0", probeRequestProtocol)
	}
}

func TestPrepareThroughH2ProxyToH1Backend(t *testing.T) {
	var backendProtocol string
	backendMux := http.NewServeMux()
	backendMux.HandleFunc("/preflight", func(w http.ResponseWriter, r *http.Request) {
		_ = json.MarshalWrite(w, wire.Preflight{Server: wire.ServerInfo{Name: "proxied"}, EngineVersion: "test", Generation: "test", Capabilities: wire.Capabilities{
			ThroughputTargets: []wire.ThroughputTarget{{Origin: ".", Protocol: "negotiated", Transport: wire.TransportFetchStream}},
			LatencyTargets:    []wire.LatencyTarget{{Origin: ".", Transport: wire.TransportWebSocket}},
		}})
	})
	backendMux.HandleFunc("/probe", func(w http.ResponseWriter, r *http.Request) {
		backendProtocol = r.Proto
		_ = json.MarshalWrite(w, wire.Probe{ClientIP: "127.0.0.1", ClientIPVersion: 4, ClientIPSource: "socket", ProtocolNegotiated: "http/1.1"})
	})
	backendMux.Handle("/ws/ping", echoPingHandler())
	backend := httptest.NewServer(backendMux)
	defer backend.Close()
	upstream, _ := url.Parse(backend.URL)
	proxy := httptest.NewUnstartedServer(httputil.NewSingleHostReverseProxy(upstream))
	proxy.EnableHTTP2 = true
	proxy.StartTLS()
	defer proxy.Close()

	cfg := DefaultConfig()
	cfg.BaseURL, cfg.InsecureSkipTLSVerify = proxy.URL, true
	prepared, err := Prepare(t.Context(), cfg)
	if err != nil {
		t.Fatal(err)
	}
	if prepared.ThroughputTarget.Protocol != "http2" {
		t.Fatalf("client-to-proxy protocol = %q", prepared.ThroughputTarget.Protocol)
	}
	if got := prepared.Preflight.Capabilities.ThroughputTargets[0].Protocol; got != "negotiated" {
		t.Fatalf("advertised protocol mutated to %q", got)
	}
	if prepared.Probe.ProtocolNegotiated != "http/1.1" || backendProtocol != "HTTP/1.1" {
		t.Fatalf("backend evidence = %q, request = %q", prepared.Probe.ProtocolNegotiated, backendProtocol)
	}
}

func TestPrepareErrorRetainsDiscoveredTargets(t *testing.T) {
	mux := http.NewServeMux()
	mux.HandleFunc("/preflight", func(w http.ResponseWriter, _ *http.Request) {
		_ = json.MarshalWrite(w, wire.Preflight{
			Generation: "test", Capabilities: wire.Capabilities{ThroughputTargets: []wire.ThroughputTarget{
				testTransfer("one", "http://one.example", "negotiated", false),
				testTransfer("two", "http://two.example", "negotiated", false),
			}},
		})
	})
	srv := httptest.NewServer(mux)
	defer srv.Close()

	cfg := DefaultConfig()
	cfg.BaseURL = srv.URL
	cfg.Stages = StageSet{Download: true}
	_, err := Prepare(t.Context(), cfg)
	preparationErr, ok := errors.AsType[*PreparationError](err)
	if !ok {
		t.Fatalf("Prepare error = %T %v, want PreparationError", err, err)
	}
	if got := len(preparationErr.Preflight.Capabilities.ThroughputTargets); got != 2 {
		t.Fatalf("retained throughput targets = %d, want 2", got)
	}
}

func newLatencyOnlyServer(t *testing.T) *httptest.Server {
	t.Helper()
	mux := http.NewServeMux()
	mountDiscovery(mux)
	mux.Handle("/ws/ping", echoPingHandler())
	return httptest.NewServer(mux)
}

func TestRunLatencyStageEndToEnd(t *testing.T) {
	srv := newLatencyOnlyServer(t)
	defer srv.Close()

	cfg := Config{
		BaseURL:         srv.URL,
		Stages:          StageSet{Latency: true},
		Warmup:          0,
		LatencyDuration: captureWindow,
		PingInterval:    20 * time.Millisecond,
		TransferStreams: TransferStreamPolicy{Forced: 1},
	}

	var mu sync.Mutex
	var results []Result
	ctx, cancel := context.WithTimeout(t.Context(), 10*time.Second)
	defer cancel()
	err := Run(ctx, cfg, func(e Event) {
		if e.Kind == EventResult {
			mu.Lock()
			results = append(results, *e.Result)
			mu.Unlock()
		}
	})
	if err != nil {
		t.Fatalf("Run: %v", err)
	}
	mu.Lock()
	defer mu.Unlock()
	if len(results) != 1 || results[0].Latency.Count == 0 {
		t.Fatalf("want one latency Result with samples, got %+v", results)
	}
}

func TestRunStopsPromptlyOnContextCancel(t *testing.T) {
	srv := newDownloadOnlyServer(t)
	defer srv.Close()

	cfg := Config{
		BaseURL:                srv.URL,
		Stages:                 StageSet{Download: true},
		Warmup:                 3 * time.Second,
		DownloadDuration:       3 * time.Second,
		TransferStreams:        TransferStreamPolicy{Forced: 1},
		DownloadBytesPerStream: 128 * 1024,
	}

	ctx, cancel := context.WithCancel(t.Context())
	time.AfterFunc(150*time.Millisecond, cancel)
	defer cancel()

	var terminal []Event
	done := make(chan error, 1)
	start := time.Now()
	go func() {
		done <- Run(ctx, cfg, func(e Event) {
			if e.Kind == EventDone {
				terminal = append(terminal, e)
			}
		})
	}()

	select {
	case err := <-done:
		if !errors.Is(err, context.Canceled) {
			t.Fatalf("Run err = %v, want context.Canceled", err)
		}
	case <-time.After(3 * time.Second):
		t.Fatal("Run did not stop after the context was cancelled")
	}
	if elapsed := time.Since(start); elapsed > 2*time.Second {
		t.Errorf("Run took %v to stop after cancellation, want well under the 3s stage duration", elapsed)
	}
	if len(terminal) != 1 || !errors.Is(terminal[0].Err, context.Canceled) {
		t.Fatalf("canceled terminal outcome = %+v", terminal)
	}
}

func newBidirectionalServer(t *testing.T) *httptest.Server {
	t.Helper()
	var uploaded atomic.Uint64
	started := time.Now()

	mux := http.NewServeMux()
	mountDiscovery(mux)
	mux.HandleFunc("/download", func(w http.ResponseWriter, r *http.Request) {
		n, err := strconv.ParseInt(r.URL.Query().Get("bytes"), 10, 64)
		if err != nil || n <= 0 {
			n = 64 * 1024
		}
		w.Header().Set("Content-Type", "application/octet-stream")
		_, _ = w.Write(make([]byte, n))
	})
	mux.HandleFunc("/upload/session", func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		_ = json.MarshalWrite(w, uploadSessionResponse{UploadID: "bidi-upload"})
	})
	mux.HandleFunc("/upload", func(w http.ResponseWriter, r *http.Request) {
		buf := make([]byte, 32*1024)
		for {
			n, err := r.Body.Read(buf)
			if n > 0 {
				uploaded.Add(uint64(n))
			}
			if err != nil {
				return
			}
		}
	})
	mountFakeProgress(mux, &uploaded, started)
	return httptest.NewServer(mux)
}

func TestRunBidirectionalStageEndToEnd(t *testing.T) {
	t.Parallel()
	cases := []struct {
		name     string
		streams  int
		duration time.Duration
	}{
		{"single stream", 1, 500 * time.Millisecond},
		{"clamped to the max of 128", 999, 2 * time.Second},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			srv := newBidirectionalServer(t)
			defer srv.Close()

			cfg := Config{
				BaseURL:                srv.URL,
				Stages:                 StageSet{Bidirectional: true},
				Warmup:                 0,
				BidirectionalDuration:  c.duration,
				TransferStreams:        TransferStreamPolicy{Forced: c.streams},
				DownloadBytesPerStream: 16 * 1024,
			}

			var mu sync.Mutex
			var results []Result
			ctx, cancel := context.WithTimeout(t.Context(), 10*time.Second)
			defer cancel()
			if err := Run(ctx, cfg, func(e Event) {
				if e.Kind == EventResult {
					mu.Lock()
					results = append(results, *e.Result)
					mu.Unlock()
				}
			}); err != nil {
				t.Fatalf("Run: %v", err)
			}

			mu.Lock()
			defer mu.Unlock()
			var sawDown, sawUp bool
			for _, res := range results {
				if res.Stage != "bidirectional" {
					t.Errorf("Result.Stage = %q, want bidirectional", res.Stage)
				}
				switch res.Direction {
				case Down:
					sawDown = true
					if res.TotalBytes == 0 {
						t.Error("download lane reported zero bytes")
					}
				case Up:
					sawUp = true
					if res.TotalBytes == 0 {
						t.Error("upload lane reported zero bytes")
					}
				}
			}
			if !sawDown || !sawUp {
				t.Fatalf("want both a download and an upload Result, got %+v", results)
			}
		})
	}
}

func TestTransferStagesOpenTheirOwnDirectionsLanes(t *testing.T) {
	var uploaded atomic.Uint64
	var mu sync.Mutex
	lanes := map[Direction]map[string]bool{Down: {}, Up: {}}
	note := func(dir Direction, r *http.Request) {
		mu.Lock()
		lanes[dir][r.URL.Query().Get("lane")] = true
		mu.Unlock()
	}

	mux := http.NewServeMux()
	mux.HandleFunc("/download", func(w http.ResponseWriter, r *http.Request) {
		note(Down, r)
		w.Header().Set("Content-Type", "application/octet-stream")
		_, _ = w.Write(make([]byte, 16*1024))
	})
	mux.HandleFunc("/upload/session", func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		_ = json.MarshalWrite(w, uploadSessionResponse{UploadID: "lane-count"})
	})
	mux.HandleFunc("/upload", func(w http.ResponseWriter, r *http.Request) {
		note(Up, r)
		buf := make([]byte, 32*1024)
		for {
			n, err := r.Body.Read(buf)
			if n > 0 {
				uploaded.Add(uint64(n))
			}
			if err != nil {
				return
			}
		}
	})
	mountFakeProgress(mux, &uploaded, time.Now())
	srv := httptest.NewServer(mux)
	defer srv.Close()

	cfg := Config{BaseURL: srv.URL, DownloadBytesPerStream: 16 * 1024}.normalized()
	reported := map[Direction]int{}
	r := &runner{cfg: cfg, streams: streamCounts{down: 1, up: 4}, http: srv.Client(), emit: func(e Event) {
		if e.Kind != EventThroughput {
			return
		}
		mu.Lock()
		reported[e.Direction] = e.Throughput.StreamCount
		mu.Unlock()
	}}

	ctx, cancel := context.WithTimeout(t.Context(), 10*time.Second)
	defer cancel()
	if err := r.runTransferStage(ctx, "bidirectional", []Direction{Down, Up}, captureWindow); err != nil {
		t.Fatalf("runTransferStage: %v", err)
	}

	mu.Lock()
	defer mu.Unlock()
	for _, c := range []struct {
		dir  Direction
		want int
	}{{Down, 1}, {Up, 4}} {
		if got := len(lanes[c.dir]); got != c.want {
			t.Errorf("%s opened %d lanes, want %d", c.dir, got, c.want)
		}
		if got := reported[c.dir]; got != c.want {
			t.Errorf("%s reported StreamCount %d, want %d", c.dir, got, c.want)
		}
	}
}

func TestRunTransferStageFanInErrorCancelsSiblingLane(t *testing.T) {
	var downloadBytesServed atomic.Int64
	mux := http.NewServeMux()
	mux.HandleFunc("/download", func(w http.ResponseWriter, r *http.Request) {
		n, err := strconv.ParseInt(r.URL.Query().Get("bytes"), 10, 64)
		if err != nil || n <= 0 {
			n = 64 * 1024
		}
		downloadBytesServed.Add(n)
		w.Header().Set("Content-Type", "application/octet-stream")
		_, _ = w.Write(make([]byte, n))
	})
	mux.HandleFunc("/upload/session", func(w http.ResponseWriter, r *http.Request) {
		time.Sleep(150 * time.Millisecond)
		w.WriteHeader(http.StatusInternalServerError)
	})
	srv := httptest.NewServer(mux)
	defer srv.Close()

	cfg := Config{BaseURL: srv.URL, TransferStreams: TransferStreamPolicy{Forced: 1}, DownloadBytesPerStream: 64 * 1024}.normalized()
	var mu sync.Mutex
	var events []Event
	r := &runner{cfg: cfg, streams: streamCounts{down: 1, up: 1}, http: srv.Client(), emit: func(e Event) { mu.Lock(); events = append(events, e); mu.Unlock() }}

	ctx, cancel := context.WithTimeout(t.Context(), 5*time.Second)
	defer cancel()
	begin := time.Now()
	err := r.runTransferStage(ctx, "bidirectional", []Direction{Down, Up}, 3*time.Second)
	if err == nil {
		t.Fatal("want an error surfaced from the failed upload session mint")
	}
	if !strings.Contains(err.Error(), "500") {
		t.Errorf("err = %v, want it to mention the upload session's HTTP 500", err)
	}
	if elapsed := time.Since(begin); elapsed > 2*time.Second {
		t.Errorf("runTransferStage took %v to return after a sibling lane errored, want prompt cancellation well under the 3s duration", elapsed)
	}
	mu.Lock()
	defer mu.Unlock()
	for _, event := range events {
		if event.Kind != EventStage || event.Phase != StagePreparing {
			t.Fatalf("preparation failure published measured data: %+v", event)
		}
	}
	if downloadBytesServed.Load() == 0 {
		t.Error("download lane made no progress before the sibling upload lane's error; test did not exercise cancellation of a priming direction")
	}
}

func TestLoadedLatencyResultPrecedesTheTransferResult(t *testing.T) {
	mux := http.NewServeMux()
	mux.Handle("/ws/ping", echoPingHandler())
	mux.HandleFunc("/download", func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/octet-stream")
		_, _ = w.Write(make([]byte, 64*1024))
	})
	srv := httptest.NewServer(mux)
	defer srv.Close()

	cfg := Config{
		BaseURL:                srv.URL,
		Warmup:                 0,
		LoadedLatency:          true,
		PingInterval:           20 * time.Millisecond,
		TransferStreams:        TransferStreamPolicy{Forced: 1},
		DownloadBytesPerStream: 64 * 1024,
	}.normalized()

	var mu sync.Mutex
	var results []Result
	r := &runner{cfg: cfg, streams: streamCounts{down: 1}, http: srv.Client(), emit: func(e Event) {
		if e.Kind != EventResult {
			return
		}
		mu.Lock()
		results = append(results, *e.Result)
		mu.Unlock()
	}}
	attachTestLatencyTarget(r, srv.URL)

	ctx, cancel := context.WithTimeout(t.Context(), 10*time.Second)
	defer cancel()
	if err := r.runTransferStage(ctx, "download", []Direction{Down}, captureWindow); err != nil {
		t.Fatalf("runTransferStage: %v", err)
	}

	mu.Lock()
	defer mu.Unlock()
	if len(results) != 2 {
		t.Fatalf("stage published %d result(s), want the transfer result and its loaded-latency companion: %+v", len(results), results)
	}
	if results[0].Latency.Count == 0 || results[0].TotalBytes != 0 {
		t.Errorf("first result = %+v, want the loaded-latency one", results[0])
	}
	if results[1].Direction != Down || results[1].TotalBytes == 0 {
		t.Errorf("last result = %+v, want the download transfer one", results[1])
	}
}

/* ---- shared WS test fixture ---- */

// echoPingHandler answers every PING with an immediate PONG, echoing the id.
func echoPingHandler() http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		conn, err := websocket.Accept(w, r, &websocket.AcceptOptions{CompressionMode: websocket.CompressionDisabled})
		if err != nil {
			return
		}
		defer conn.Close(websocket.StatusNormalClosure, "")
		ctx := r.Context()
		for {
			_, msg, err := conn.Read(ctx)
			if err != nil {
				return
			}
			f, derr := wire.Decode(string(msg))
			if derr != nil {
				continue
			}
			if f.Op == wire.OpHI {
				if err := conn.Write(ctx, websocket.MessageText, []byte(wire.Encode(wire.Frame{Op: wire.OpREADY}))); err != nil {
					return
				}
				continue
			}
			if f.Op != wire.OpPING {
				continue
			}
			pong := wire.Encode(wire.Frame{Op: wire.OpPONG, ID: f.ID, Nanos: uint64(time.Now().UnixNano())})
			if err := conn.Write(ctx, websocket.MessageText, []byte(pong)); err != nil {
				return
			}
		}
	})
}

func newEchoPingServer(t *testing.T) *httptest.Server {
	t.Helper()
	mux := http.NewServeMux()
	mux.Handle("/ws/ping", echoPingHandler())
	return httptest.NewServer(mux)
}

func TestLatencyBusEvidenceNamesTheBusNotTheProbe(t *testing.T) {
	probe := &wire.Probe{ProtocolNegotiated: "HTTP/1.1"}
	wt := &wire.LatencyTarget{Transport: wire.TransportWebTransport}
	if got, want := latencyBusEvidence(wt, probe), "h3"; got != want {
		t.Errorf("WebTransport bus evidence = %q, want %q", got, want)
	}
	// The WebSocket bus really is the HTTP/1.1 the probe observed: its Upgrade handshake cannot ride anything else.
	ws := &wire.LatencyTarget{Transport: wire.TransportWebSocket}
	if got, want := latencyBusEvidence(ws, probe), "HTTP/1.1"; got != want {
		t.Errorf("WebSocket bus evidence = %q, want %q", got, want)
	}
	if got := latencyBusEvidence(ws, nil); got != "" {
		t.Errorf("unprobed WebSocket bus evidence = %q, want empty", got)
	}
}

func TestConnectionSummaryNamesEveryPathTheSameWay(t *testing.T) {
	for _, c := range []struct{ transport, protocol, want string }{
		{wire.TransportFetchStream, "http1", "Fetch stream · HTTP/1.1 · TLS"},
		{wire.TransportFetchStream, "negotiated", "Fetch stream · Negotiated · TLS"},
		{wire.TransportWebSocket, "http1", "WebSocket · HTTP/1.1 · TLS"},
		{wire.TransportWebTransport, "http3", "WebTransport · HTTP/3 · TLS"},
		{wire.TransportWebTransportDatagram, "http3", "WebTransport datagrams · HTTP/3 · TLS"},
		{wire.TransportWebTransport, "h3", "WebTransport · HTTP/3 · TLS"},
		{wire.TransportFetchStream, "", "Fetch stream · -- · TLS"},
	} {
		if got := ConnectionSummary(c.transport, c.protocol, true); got != c.want {
			t.Errorf("ConnectionSummary(%q, %q) = %q, want %q", c.transport, c.protocol, got, c.want)
		}
	}
	if got, want := ConnectionSummary(wire.TransportFetchStream, "http1", false), "Fetch stream · HTTP/1.1 · clear"; got != want {
		t.Errorf("clear summary = %q, want %q", got, want)
	}
}

func TestLoadedLatencyPublishesTimeoutOnlyAndUnresolvedResults(t *testing.T) {
	for _, duration := range []time.Duration{80 * time.Millisecond, 400 * time.Millisecond} {
		t.Run(duration.String(), func(t *testing.T) {
			transfer := newBytesEchoDownloadServer()
			defer transfer.Close()
			ping := newSilentPingServer(t)
			defer ping.Close()
			cfg := Config{BaseURL: transfer.URL, LoadedLatency: true, PingInterval: 10 * time.Millisecond, TransferStreams: TransferStreamPolicy{Forced: 1}, DownloadBytesPerStream: 64 * 1024}.normalized()
			var results []Result
			r := &runner{cfg: cfg, streams: streamCounts{down: 1}, http: transfer.Client(), emit: func(e Event) {
				if e.Kind == EventResult {
					results = append(results, *e.Result)
				}
			}}
			attachTestLatencyTarget(r, ping.URL)
			if err := r.runTransferStage(t.Context(), "download", []Direction{Down}, duration); err != nil {
				t.Fatal(err)
			}
			if len(results) != 2 || results[0].Direction != "" || results[1].Direction != Down {
				t.Fatalf("loaded results: %+v", results)
			}
			stats := results[0].Latency
			if stats.Count != 0 || stats.Unresolved == 0 {
				t.Fatalf("missing unresolved population: %+v", stats)
			}
			if duration > stats.TimeoutAfter && stats.Timeouts == 0 {
				t.Fatalf("missing timeout population: %+v", stats)
			}
		})
	}
}
