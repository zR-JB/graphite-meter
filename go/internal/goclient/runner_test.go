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
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

func TestAdaptiveWarmup(t *testing.T) {
	t.Parallel()
	const base = 500 * time.Millisecond
	for _, c := range []struct{ rtt, want time.Duration }{
		{0, base},
		{10 * time.Millisecond, base},
		{100 * time.Millisecond, time.Second},
		{time.Second, 4 * time.Second},
	} {
		if got := adaptiveWarmup(base, c.rtt); got != c.want {
			t.Errorf("adaptiveWarmup(%v, %v) = %v, want %v", base, c.rtt, got, c.want)
		}
	}
}

func TestRunLatencyStageCapturesIdleRTT(t *testing.T) {
	t.Parallel()
	r := testRunner(newPingServer(t, answerAll, 0))
	r.cfg.PingInterval = 20 * time.Millisecond
	if err := r.runTestStage(t.Context(), StageLatency, captureWindow); err != nil || r.idleRTT <= 0 {
		t.Fatalf("latency stage = %v, idle RTT %v", err, r.idleRTT)
	}
}

func TestRunStagesEndToEnd(t *testing.T) {
	t.Parallel()
	for _, c := range []struct {
		stages     StageSet
		streams    int
		directions []Direction
	}{
		{StageSet{Latency: true}, 1, nil},
		{StageSet{Download: true}, 1, []Direction{Down}},
		{StageSet{Bidirectional: true}, 1, []Direction{Down, Up}},
		{StageSet{Bidirectional: true}, 2, []Direction{Down, Up}},
	} {
		t.Run(fmt.Sprintf("%s/%d", c.stages.name(), c.streams), func(t *testing.T) {
			t.Parallel()
			srv := newTransferServer(t)
			cfg := Config{
				BaseURL:               srv.URL,
				Stages:                c.stages,
				Warmup:                0,
				LatencyDuration:       captureWindow,
				DownloadDuration:      time.Second,
				BidirectionalDuration: time.Second,
				PingInterval:          20 * time.Millisecond,
				TransferStreams:       TransferStreamPolicy{Forced: c.streams},
			}
			var mu sync.Mutex
			var events []Event
			if err := runDirect(t.Context(), cfg, func(e Event) {
				mu.Lock()
				defer mu.Unlock()
				events = append(events, e)
			}); err != nil {
				t.Fatalf("run: %v", err)
			}
			mu.Lock()
			defer mu.Unlock()
			var directions []Direction
			for _, e := range events {
				if e.Kind == EventResult {
					directions = append(directions, e.Direction)
					if e.Result.Unavailable ||
						e.Result.TotalBytes == 0 ||
						e.Result.Samples == 0 ||
						e.Result.MeanBps <= 0 {
						t.Fatalf("result lacks a measured window: %+v", e.Result)
					}
				}
			}
			done := events[len(events)-1]
			if !slices.Equal(directions, c.directions) || done.Kind != EventDone || done.Outcome() != OutcomeComplete {
				t.Fatalf("directions=%v terminal=%+v", directions, done)
			}
			if c.stages.Latency || cfg.LoadedLatency {
				results := done.Servers.Servers[0].Results
				i := slices.IndexFunc(results, func(r Result) bool { return r.Direction == "" })
				if i < 0 || results[i].Latency.Count == 0 {
					t.Fatalf("no latency population: %+v", results)
				}
			}
		})
	}
}

func (s StageSet) name() string {
	var names []string
	for _, stage := range (Config{Stages: s}).Plan() {
		names = append(names, string(stage.Name))
	}
	return strings.Join(names, "+")
}

func TestRunStopsPromptlyOnContextCancel(t *testing.T) {
	t.Parallel()
	srv := newTransferServer(t)
	cfg := Config{
		BaseURL:          srv.URL,
		Stages:           StageSet{Download: true},
		Warmup:           3 * time.Second,
		DownloadDuration: 3 * time.Second,
		TransferStreams:  TransferStreamPolicy{Forced: 1},
	}
	ctx, cancel := context.WithCancel(t.Context())
	time.AfterFunc(150*time.Millisecond, cancel)
	var terminal []Event
	started := time.Now()
	err := runDirect(ctx, cfg, func(e Event) {
		if e.Kind == EventDone {
			terminal = append(terminal, e)
		}
	})
	if !errors.Is(err, context.Canceled) || time.Since(started) > 2*time.Second {
		t.Fatalf("run returned %v after %v, want a prompt context.Canceled", err, time.Since(started))
	}
	if len(terminal) != 1 || terminal[0].Outcome() != OutcomeStopped {
		t.Fatalf("cancelled terminal outcome = %+v", terminal)
	}
}

func TestRunAcceptsProxyProtocolBoundary(t *testing.T) {
	t.Parallel()
	var origin string
	var probeRequestProtocol atomic.Value
	mux := http.NewServeMux()
	mux.HandleFunc("/preflight", func(w http.ResponseWriter, _ *http.Request) {
		_ = json.MarshalWrite(w, wire.Preflight{
			Generation: "test", Server: wire.ServerInfo{Name: "proxy"},
			Capabilities: wire.Capabilities{
				ThroughputTargets: []wire.ThroughputTarget{testTransfer("http2", origin, "http2", true)},
			},
		})
	})
	mux.HandleFunc("/probe", func(w http.ResponseWriter, r *http.Request) {
		probeRequestProtocol.Store(r.Proto)
		writeProbe(w, r)
	})
	mux.HandleFunc("/download", writeDownload)
	srv := httptest.NewUnstartedServer(mux)
	srv.EnableHTTP2 = true
	srv.StartTLS()
	defer srv.Close()
	origin = srv.URL

	cfg := Config{
		BaseURL:               origin,
		Stages:                StageSet{Download: true},
		DownloadDuration:      100 * time.Millisecond,
		InsecureSkipTLSVerify: true,
		TransferStreams:       TransferStreamPolicy{Forced: 1},
	}
	if err := runDirect(t.Context(), cfg, func(Event) {}); err != nil {
		t.Fatalf("run through an H2 proxy with H1 downstream evidence: %v", err)
	}
	if got := probeRequestProtocol.Load(); got != "HTTP/2.0" {
		t.Fatalf("client-to-proxy probe used %q, want HTTP/2.0", got)
	}
}

func TestPrepareThroughH2ProxyToH1Backend(t *testing.T) {
	t.Parallel()
	var backendProtocol atomic.Value
	backendMux := http.NewServeMux()
	backendMux.HandleFunc("/preflight", func(w http.ResponseWriter, _ *http.Request) {
		_ = json.MarshalWrite(w, wire.Preflight{Server: wire.ServerInfo{
			Name: "proxied",
		}, EngineVersion: "test", Generation: "test", Capabilities: wire.Capabilities{
			ThroughputTargets: []wire.ThroughputTarget{
				{Origin: ".", Protocol: "negotiated", Transport: wire.TransportFetchStream},
			},
			LatencyTargets: []wire.LatencyTarget{{Origin: ".", Transport: wire.TransportWebSocket}},
		}})
	})
	backendMux.HandleFunc("/probe", func(w http.ResponseWriter, r *http.Request) {
		backendProtocol.Store(r.Proto)
		writeProbe(w, r)
	})
	backendMux.Handle("/ws/ping", pingHandler(answerAll, 0))
	backend := httptest.NewServer(backendMux)
	defer backend.Close()
	upstream, _ := url.Parse(backend.URL)
	proxy := httptest.NewUnstartedServer(httputil.NewSingleHostReverseProxy(upstream))
	proxy.EnableHTTP2 = true
	proxy.StartTLS()
	defer proxy.Close()

	cfg := DefaultConfig()
	cfg.BaseURL, cfg.InsecureSkipTLSVerify = proxy.URL, true
	prepared, err := prepare(t.Context(), cfg)
	if err != nil {
		t.Fatal(err)
	}
	if prepared.ThroughputTarget.Protocol != "http2" ||
		prepared.Preflight.Capabilities.ThroughputTargets[0].Protocol != "negotiated" {
		t.Fatalf(
			"client-to-proxy protocol = %q; advertised %q",
			prepared.ThroughputTarget.Protocol,
			prepared.Preflight.Capabilities.ThroughputTargets[0].Protocol,
		)
	}
	if got := backendProtocol.Load(); got != "HTTP/1.1" {
		t.Fatalf("backend request = %q, want HTTP/1.1 behind the HTTP/2 proxy", got)
	}
}

func TestPrepareErrorRetainsDiscoveredTargets(t *testing.T) {
	t.Parallel()
	srv := httptest.NewServer(ambiguousFetch())
	defer srv.Close()
	cfg := DefaultConfig()
	cfg.BaseURL, cfg.Stages = srv.URL, StageSet{Download: true}
	_, err := prepare(t.Context(), cfg)
	if preparationErr, ok := errors.AsType[*PreparationError](err); !ok ||
		len(preparationErr.Preflight.Capabilities.ThroughputTargets) != 2 {
		t.Fatalf("prepare error = %T %v, want a PreparationError with both discovered targets", err, err)
	}
}

func TestTransferStagesOpenTheirOwnDirectionsLanes(t *testing.T) {
	t.Parallel()
	var mu sync.Mutex
	lanes := map[string]map[string]bool{"/download": {}, "/upload": {}}
	transfer := newTransferServer(t)
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		mu.Lock()
		if seen, ok := lanes[r.URL.Path]; ok {
			seen[r.URL.Query().Get("lane")] = true
		}
		mu.Unlock()
		transfer.Config.Handler.ServeHTTP(w, r)
	}))
	defer srv.Close()
	r := testRunner(srv)
	r.streams = streamCounts{down: 1, up: 4}
	if err := r.runTestStage(t.Context(), StageBidirectional, captureWindow); err != nil {
		t.Fatalf("coordinated transfer stage: %v", err)
	}
	mu.Lock()
	defer mu.Unlock()
	if len(lanes["/download"]) != 1 || len(lanes["/upload"]) != 4 {
		t.Fatalf("opened %d download and %d upload lanes, want 1 and 4", len(lanes["/download"]), len(lanes["/upload"]))
	}
}

func TestRunTransferStageFanInErrorCancelsSiblingLane(t *testing.T) {
	t.Parallel()
	var downloadBytesServed atomic.Int64
	mux := http.NewServeMux()
	mux.HandleFunc("/download", func(w http.ResponseWriter, r *http.Request) {
		downloadBytesServed.Add(64 * 1024)
		writeDownload(w, r)
	})
	mux.HandleFunc("/upload/session", func(w http.ResponseWriter, _ *http.Request) {
		time.Sleep(150 * time.Millisecond)
		w.WriteHeader(http.StatusInternalServerError)
	})
	srv := httptest.NewServer(mux)
	defer srv.Close()

	var mu sync.Mutex
	var events []Event
	r := testRunner(srv)
	r.emit = func(e Event) {
		mu.Lock()
		defer mu.Unlock()
		events = append(events, e)
	}
	started := time.Now()
	err := r.runTestStage(t.Context(), StageBidirectional, 3*time.Second)
	if err == nil || !strings.Contains(err.Error(), "500") {
		t.Fatalf("err = %v, want the upload session's HTTP 500", err)
	}
	if elapsed := time.Since(started); elapsed > 2*time.Second {
		t.Errorf("stage took %v to return after a sibling lane errored, want prompt cancellation", elapsed)
	}
	mu.Lock()
	defer mu.Unlock()
	for _, event := range events {
		if event.Kind == EventThroughput ||
			event.Kind == EventLatency ||
			event.Kind == EventResult ||
			event.Kind == EventStage && event.Phase != PhasePreparing {
			t.Fatalf("preparation failure published measured data: %+v", event)
		}
	}
	if downloadBytesServed.Load() == 0 {
		t.Error("download made no progress before the upload failed; sibling cancellation was not exercised")
	}
}

func TestConnectionSummaryNamesEveryPathTheSameWay(t *testing.T) {
	t.Parallel()
	for _, c := range []struct {
		transport, protocol string
		tls                 bool
		want                string
	}{
		{wire.TransportFetchStream, "http1", true, "Fetch stream · HTTP/1.1 · TLS"},
		{wire.TransportFetchStream, "http1", false, "Fetch stream · HTTP/1.1 · clear"},
		{wire.TransportFetchStream, "negotiated", true, "Fetch stream · Negotiated · TLS"},
		{wire.TransportWebSocket, "http1", true, "WebSocket · HTTP/1.1 · TLS"},
		{wire.TransportWebTransport, "h3", true, "WebTransport · HTTP/3 · TLS"},
		{wire.TransportWebTransportDatagram, "http3", true, "WebTransport datagrams · HTTP/3 · TLS"},
	} {
		if got := ConnectionSummary(c.transport, c.protocol, c.tls); got != c.want {
			t.Errorf("ConnectionSummary(%q, %q, %t) = %q, want %q", c.transport, c.protocol, c.tls, got, c.want)
		}
	}
}

func TestLoadedLatencyDrainsSilentProbesToTimeouts(t *testing.T) {
	t.Parallel()
	for _, duration := range []time.Duration{80 * time.Millisecond, 400 * time.Millisecond} {
		t.Run(duration.String(), func(t *testing.T) {
			t.Parallel()
			var details *RunDetails
			var transferResult *Result
			r := testRunner(newTransferServer(t))
			r.latencyTarget = new(testChannel("silent", newPingServer(t, answerNone, 0).URL, false))
			r.cfg.LoadedLatency, r.cfg.PingInterval = true, 10*time.Millisecond
			r.emit = func(e Event) {
				if e.Kind == EventResult {
					transferResult = e.Result
				}
				if e.Kind == EventDone {
					details = e.Servers
				}
			}
			if err := r.runTestStage(t.Context(), StageDownload, duration); err != nil {
				t.Fatal(err)
			}
			results := details.Servers[0].Results
			if transferResult == nil ||
				transferResult.Direction != Down ||
				len(results) != 2 ||
				results[0].Direction != "" {
				t.Fatalf("loaded results: %+v %+v", transferResult, results)
			}
			stats := results[0].Latency
			if stats.Count != 0 || stats.Unresolved != 0 || stats.Timeouts == 0 {
				t.Fatalf("silent loaded probes did not drain to timeouts: %+v", stats)
			}
		})
	}
}
