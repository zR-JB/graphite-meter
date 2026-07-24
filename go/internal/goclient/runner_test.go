package goclient

import (
	"context"
	"encoding/json"
	"errors"
	"net/http"
	"net/http/httptest"
	"net/http/httputil"
	"net/url"
	"strconv"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"github.com/coder/websocket"
	"github.com/quic-go/quic-go/http3"
	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

// captureWindow is the stage duration in tests that assert traffic landed:
// wide enough that a contended CI runner still records samples, so the
// assertions test capture rather than machine speed.
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
			r := &runner{cfg: Config{Warmup: c.warmup}, streams: c.streams, idleRTT: c.idleRTT}
			if got := r.laneStaggerStep(); got != c.want {
				t.Errorf("laneStaggerStep() = %v, want %v", got, c.want)
			}
		})
	}
}

func TestStaggerSleep(t *testing.T) {
	t.Run("lane 0 returns immediately", func(t *testing.T) {
		start := time.Now()
		if !staggerSleep(context.Background(), 0, time.Hour) {
			t.Fatal("want true for lane 0")
		}
		if time.Since(start) > 100*time.Millisecond {
			t.Error("lane 0 should not wait regardless of step")
		}
	})

	t.Run("zero step returns immediately", func(t *testing.T) {
		if !staggerSleep(context.Background(), 5, 0) {
			t.Fatal("want true for a zero step")
		}
	})

	t.Run("waits out the delay then succeeds", func(t *testing.T) {
		start := time.Now()
		if !staggerSleep(context.Background(), 2, 20*time.Millisecond) {
			t.Fatal("want true once the delay elapses")
		}
		if elapsed := time.Since(start); elapsed < 30*time.Millisecond {
			t.Errorf("returned after %v, want at least ~40ms (lane 2 * step 20ms)", elapsed)
		}
	})

	t.Run("cancelled context aborts the wait", func(t *testing.T) {
		ctx, cancel := context.WithCancel(context.Background())
		cancel()
		if staggerSleep(ctx, 3, time.Hour) {
			t.Fatal("want false once the context is already cancelled")
		}
	})
}

func TestRunnerFail(t *testing.T) {
	t.Run("nil error is passed through without emitting", func(t *testing.T) {
		r := &runner{emit: func(Event) { t.Fatal("emit should not be called for a nil error") }}
		if err := r.fail(nil); err != nil {
			t.Errorf("fail(nil) = %v, want nil", err)
		}
	})

	t.Run("context.Canceled is swallowed without emitting", func(t *testing.T) {
		r := &runner{emit: func(Event) { t.Fatal("emit should not be called for context.Canceled") }}
		if err := r.fail(context.Canceled); !errors.Is(err, context.Canceled) {
			t.Errorf("fail(Canceled) = %v, want context.Canceled", err)
		}
	})

	t.Run("other errors are emitted as EventError", func(t *testing.T) {
		var got []Event
		r := &runner{emit: func(e Event) { got = append(got, e) }}
		want := errors.New("boom")
		if err := r.fail(want); !errors.Is(err, want) {
			t.Errorf("fail(boom) = %v, want boom", err)
		}
		if len(got) != 1 || got[0].Kind != EventError || got[0].Err != want {
			t.Errorf("emitted events = %+v, want a single EventError wrapping %v", got, want)
		}
	})
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

func TestWarmupGate(t *testing.T) {
	t.Run("zero warmup starts measuring immediately", func(t *testing.T) {
		var mu sync.Mutex
		var events []Event
		r := &runner{cfg: Config{Warmup: 0}, emit: func(e Event) { mu.Lock(); events = append(events, e); mu.Unlock() }}
		start := r.warmupGate(context.Background(), "download")
		select {
		case <-start:
		default:
			t.Fatal("start channel should already be closed when warmup is zero")
		}
		mu.Lock()
		defer mu.Unlock()
		if len(events) != 1 || events[0].Kind != EventStage || events[0].Message != "measure" {
			t.Errorf("events = %+v, want a single measure stage event", events)
		}
	})

	t.Run("positive warmup gates start until the timer fires", func(t *testing.T) {
		var mu sync.Mutex
		var messages []string
		r := &runner{cfg: Config{Warmup: 30 * time.Millisecond}, emit: func(e Event) {
			mu.Lock()
			messages = append(messages, e.Message)
			mu.Unlock()
		}}
		start := r.warmupGate(context.Background(), "download")
		select {
		case <-start:
			t.Fatal("start should not be closed before the warmup timer fires")
		default:
		}
		select {
		case <-start:
		case <-time.After(2 * time.Second):
			t.Fatal("start was never closed")
		}
		mu.Lock()
		defer mu.Unlock()
		if len(messages) != 2 || messages[0] != "warmup" || messages[1] != "measure" {
			t.Errorf("stage messages = %v, want [warmup measure]", messages)
		}
	})
}

func TestRunLatencyStageCapturesIdleRTT(t *testing.T) {
	srv := newEchoPingServer(t)
	defer srv.Close()

	cfg := Config{BaseURL: srv.URL, PingInterval: 20 * time.Millisecond}.normalized()
	r := &runner{cfg: cfg, http: srv.Client(), emit: func(Event) {}}
	attachTestLatencyTarget(r, srv.URL)

	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
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
		_ = json.NewEncoder(w).Encode(wire.Preflight{Server: wire.ServerInfo{Name: "test"}, EngineVersion: "test", Capabilities: wire.Capabilities{
			ThroughputTargets: []wire.ThroughputTarget{testTransfer("http1-clear", origin, "http1", false)},
			LatencyTargets:    []wire.LatencyTarget{testChannel("ws-http1-clear", origin, false)},
		}})
	})
	mux.HandleFunc("/probe", func(w http.ResponseWriter, r *http.Request) {
		_ = json.NewEncoder(w).Encode(wire.Probe{ClientIP: "127.0.0.1", ClientIPVersion: 4, ClientIPSource: "socket", ProtocolNegotiated: "http/1.1"})
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
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	if err := Run(ctx, cfg, func(e Event) { mu.Lock(); events = append(events, e); mu.Unlock() }); err != nil {
		t.Fatalf("Run: %v", err)
	}

	mu.Lock()
	defer mu.Unlock()
	var result *Result
	var sawComplete bool
	for _, e := range events {
		if e.Kind == EventResult && e.Stage == "download" {
			result = e.Result
		}
		if e.Kind == EventComplete {
			sawComplete = true
		}
	}
	if result == nil {
		t.Fatal("no download Result event emitted")
	}
	if result.TotalBytes == 0 {
		t.Error("download result reports zero bytes")
	}
	if !sawComplete {
		t.Error("Run did not emit EventComplete")
	}
}

func TestRunAcceptsProxyProtocolBoundary(t *testing.T) {
	var origin string
	var probeRequestProtocol string
	mux := http.NewServeMux()
	mux.HandleFunc("/preflight", func(w http.ResponseWriter, r *http.Request) {
		_ = json.NewEncoder(w).Encode(wire.Preflight{
			Server: wire.ServerInfo{Name: "proxy"},
			Capabilities: wire.Capabilities{ThroughputTargets: []wire.ThroughputTarget{
				testTransfer("http2", origin, "http2", true),
			}},
		})
	})
	mux.HandleFunc("/probe", func(w http.ResponseWriter, r *http.Request) {
		probeRequestProtocol = r.Proto
		_ = json.NewEncoder(w).Encode(wire.Probe{
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
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
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
		_ = json.NewEncoder(w).Encode(wire.Preflight{Server: wire.ServerInfo{Name: "proxied"}, EngineVersion: "test", Generation: "test", Capabilities: wire.Capabilities{
			ThroughputTargets: []wire.ThroughputTarget{{Origin: ".", Protocol: "negotiated"}},
			LatencyTargets:    []wire.LatencyTarget{{Origin: "."}},
		}})
	})
	backendMux.HandleFunc("/probe", func(w http.ResponseWriter, r *http.Request) {
		backendProtocol = r.Proto
		_ = json.NewEncoder(w).Encode(wire.Probe{ClientIP: "127.0.0.1", ClientIPVersion: 4, ClientIPSource: "socket", ProtocolNegotiated: "http/1.1"})
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
	prepared, err := Prepare(context.Background(), cfg)
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
		_ = json.NewEncoder(w).Encode(wire.Preflight{
			Capabilities: wire.Capabilities{ThroughputTargets: []wire.ThroughputTarget{
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
	_, err := Prepare(context.Background(), cfg)
	var preparationErr *PreparationError
	if !errors.As(err, &preparationErr) {
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
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
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

// TestRunStopsPromptlyOnContextCancel checks a mid-warmup cancellation unwinds
// every stage's goroutines and returns context.Canceled without emitting an
// EventError (fail() swallows cancellation as an expected shutdown, not a fault).
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

	ctx, cancel := context.WithCancel(context.Background())
	time.AfterFunc(150*time.Millisecond, cancel)
	defer cancel()

	var sawError bool
	done := make(chan error, 1)
	start := time.Now()
	go func() {
		done <- Run(ctx, cfg, func(e Event) {
			if e.Kind == EventError {
				sawError = true
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
	if sawError {
		t.Error("Run emitted EventError for a plain context cancellation")
	}
}

// newBidirectionalServer wires up every endpoint the bidirectional stage
// touches: preflight, a download echo, and the upload session/sink/progress
// trio (mirroring newFakeUploadServer in upload_test.go).
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
		_ = json.NewEncoder(w).Encode(uploadSessionResponse{UploadID: "bidi-upload"})
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

// TestRunBidirectionalStageEndToEnd checks the download and upload lanes run
// concurrently without interfering: each reports its own non-zero Result, at
// both a single forced stream and the forced-stream clamp ceiling from
// Config.normalized().
func TestRunBidirectionalStageEndToEnd(t *testing.T) {
	cases := []struct {
		name     string
		streams  int
		duration time.Duration
	}{
		{"single stream", 1, 500 * time.Millisecond},
		// Spawning 128 lanes under the race detector eats most of a half
		// second, so the clamp case gets headroom for its startup cost.
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
			ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
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

// TestRunTransferStageFanInErrorCancelsSiblingLane checks that when one lane
// of a transfer stage fails, its sibling, still actively transferring, is
// cancelled promptly rather than left running until its own duration expires.
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
		// The delay lets the download lane get well into transferring, so this
		// failure lands mid-transfer for its sibling rather than racing start-up.
		time.Sleep(150 * time.Millisecond)
		w.WriteHeader(http.StatusInternalServerError)
	})
	srv := httptest.NewServer(mux)
	defer srv.Close()

	cfg := Config{BaseURL: srv.URL, TransferStreams: TransferStreamPolicy{Forced: 1}, DownloadBytesPerStream: 64 * 1024}.normalized()
	r := &runner{cfg: cfg, streams: 1, http: srv.Client(), emit: func(Event) {}}

	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
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
	if downloadBytesServed.Load() == 0 {
		t.Error("download lane made no progress before the sibling upload lane's error; test didn't exercise a genuine mid-transfer cancellation")
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
