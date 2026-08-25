package goclient

import (
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"reflect"
	"strings"
	"testing"
	"time"

	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

func TestDefaultConfig(t *testing.T) {
	want := Config{
		BaseURL:                "http://127.0.0.1:7246",
		ThroughputTarget:       "auto",
		ThroughputProtocol:     "auto",
		ThroughputTransport:    "auto",
		LatencyTarget:          "auto",
		LatencyTransport:       "auto",
		Stages:                 StageSet{Latency: true, Download: true, Upload: true},
		Warmup:                 800 * time.Millisecond,
		LatencyDuration:        4 * time.Second,
		DownloadDuration:       10 * time.Second,
		UploadDuration:         10 * time.Second,
		BidirectionalDuration:  10 * time.Second,
		TransferStreams:        TransferStreamPolicy{AutomaticMax: 6},
		PingInterval:           250 * time.Millisecond,
		LoadedLatency:          true,
		DownloadBytesPerStream: 64 * 1024 * 1024 * 1024,
		UploadBytesPerStream:   64 * 1024 * 1024 * 1024,
		MaxIdleConnsPerHost:    256,
		ResponseHeaderTimeout:  10 * time.Second,
		ExpectContinueTimeout:  time.Second,
	}
	if got := DefaultConfig(); !reflect.DeepEqual(got, want) {
		t.Errorf("DefaultConfig() = %+v, want %+v", got, want)
	}
}

// The automatic multiplexed counts are per direction and per protocol, the same
// table the browser resolves in client/src/lib/runner/real/streamPolicy.ts. h3
// upload runs one lane: it loses 9.3% going from 1 to 4 lanes under loss, so
// the three lanes both directions once shared contradicted the measurement.
func TestTransferStreamPolicyPerDirection(t *testing.T) {
	auto := TransferStreamPolicy{AutomaticMax: 6}
	for _, c := range []struct {
		protocol string
		dir      Direction
		want     int
	}{
		{"http1", Down, 6}, {"http1", Up, 6},
		{"http2", Down, 1}, {"http2", Up, 4},
		{"http3", Down, 1}, {"http3", Up, 1},
		{"negotiated", Down, 6}, {"negotiated", Up, 6},
	} {
		if got := auto.Resolve(c.protocol, c.dir); got != c.want {
			t.Errorf("automatic %s %s streams = %d, want %d", c.protocol, c.dir, got, c.want)
		}
	}
	// Both spellings resolve: a run screen holds the negotiated evidence, and a
	// label reading "Automatic" where the lanes differ per direction reports
	// nothing at all.
	for _, c := range []struct {
		protocol, transport, want string
	}{
		{"http2", wire.TransportFetchStream, "Automatic · 1 download / 4 upload"},
		{"http3", wire.TransportFetchStream, "Automatic · 1 download / 1 upload"},
		{"http1", wire.TransportFetchStream, "Automatic · up to 6 per direction"},
		{"h2", wire.TransportFetchStream, "Automatic · 1 download / 4 upload"},
		{"h3", wire.TransportFetchStream, "Automatic · 1 download / 1 upload"},
		{"HTTP/2.0", wire.TransportFetchStream, "Automatic · 1 download / 4 upload"},
		{"http/1.1", wire.TransportFetchStream, "Automatic · up to 6 per direction"},
	} {
		if got := auto.Label(c.protocol, c.transport); got != c.want {
			t.Errorf("%s label = %q, want %q", c.protocol, got, c.want)
		}
	}
	// The stage resolves both directions from one policy, so what the label
	// reports and what the lanes open cannot drift apart.
	for _, c := range []struct {
		protocol, transport string
		want                streamCounts
	}{
		{"http2", wire.TransportFetchStream, streamCounts{down: 1, up: 4}},
		{"http3", wire.TransportFetchStream, streamCounts{down: 1, up: 1}},
		{"http1", wire.TransportFetchStream, streamCounts{down: 6, up: 6}},
		{"http3", wire.TransportWebTransport, streamCounts{down: 1, up: 1}},
	} {
		if got := auto.lanes(c.protocol, c.transport); got != c.want {
			t.Errorf("lanes(%s, %s) = %+v, want %+v", c.protocol, c.transport, got, c.want)
		}
	}
}

func TestTransferStreamPolicy(t *testing.T) {
	forced := TransferStreamPolicy{Forced: 9}
	for _, protocol := range []string{"http1", "http2", "http3"} {
		for _, dir := range []Direction{Down, Up} {
			if got := forced.Resolve(protocol, dir); got != 9 {
				t.Errorf("forced %s %s streams = %d, want 9", protocol, dir, got)
			}
		}
		if got := forced.lanes(protocol, wire.TransportFetchStream); got != (streamCounts{down: 9, up: 9}) {
			t.Errorf("forced %s lanes = %+v, want 9 per direction", protocol, got)
		}
	}
	// A forced count still clamps to what one session carries.
	if got := (TransferStreamPolicy{Forced: 99}).lanes("http3", wire.TransportWebTransport); got != (streamCounts{down: wire.WTMaxStreams, up: wire.WTMaxStreams}) {
		t.Errorf("forced webtransport lanes = %+v, want the %d cap per direction", got, wire.WTMaxStreams)
	}
	if got := forced.Label("http3", wire.TransportFetchStream); got != "Forced · 9 per direction" {
		t.Errorf("forced label = %q", got)
	}
	if got := forced.Label("http3", wire.TransportWebTransport); got != "Forced · 9 per direction" {
		t.Errorf("forced webtransport label = %q", got)
	}
	// The session carries one continuous lane per direction, not the three a
	// negotiated HTTP/3 fetch path opens.
	session := TransferStreamPolicy{AutomaticMax: 6}
	if got := session.Label("http3", wire.TransportWebTransport); got != "Automatic · 1 continuous stream per direction" {
		t.Errorf("automatic webtransport label = %q", got)
	}
	if got := session.ResolveWebTransport(); got != 1 {
		t.Errorf("automatic webtransport streams = %d, want 1", got)
	}
}

// The ping bus is the only traffic this client puts on a WebTransport session
// carrying latency, so a cadence past the server's idle bound has the bus reaped
// between pings. The knob is bound against the published contract value rather
// than a number picked client-side, so the two cannot disagree.
func TestValidatePingInterval(t *testing.T) {
	if MaxPingInterval*2 != wire.WTIdleBound {
		t.Errorf("MaxPingInterval = %v, want half of the %v idle bound", MaxPingInterval, wire.WTIdleBound)
	}
	for _, c := range []struct {
		d       time.Duration
		wantErr bool
	}{
		{250 * time.Millisecond, false},
		{MaxPingInterval, false},
		{MaxPingInterval + time.Millisecond, true},
		{wire.WTIdleBound, true},
		{45 * time.Second, true},
		{0, true},
		{-time.Second, true},
	} {
		err := ValidatePingInterval(c.d)
		if (err != nil) != c.wantErr {
			t.Errorf("ValidatePingInterval(%v) = %v, want error %t", c.d, err, c.wantErr)
		}
		if err != nil && c.d > 0 && !strings.Contains(err.Error(), MaxPingInterval.String()) {
			t.Errorf("ValidatePingInterval(%v) = %q, want it to name the %v bound", c.d, err, MaxPingInterval)
		}
	}
}

// newWTLatencyServer advertises a WebTransport latency bus and nothing else, so
// selection resolves to the transport the idle bound belongs to.
func newWTLatencyServer(t *testing.T) *httptest.Server {
	t.Helper()
	mux := http.NewServeMux()
	mux.HandleFunc("/preflight", func(w http.ResponseWriter, r *http.Request) {
		origin := "http://" + r.Host
		wtPing := testChannel("wt-ping", origin, false)
		wtPing.Transport, wtPing.Protocol = wire.TransportWebTransport, "http3"
		_ = json.NewEncoder(w).Encode(wire.Preflight{Capabilities: wire.Capabilities{
			ThroughputTargets: []wire.ThroughputTarget{testTransfer("http1-clear", origin, "http1", false)},
			LatencyTargets:    []wire.LatencyTarget{wtPing},
		}})
	})
	mux.HandleFunc("/probe", func(w http.ResponseWriter, _ *http.Request) {
		_ = json.NewEncoder(w).Encode(wire.Probe{ClientIP: "127.0.0.1", ClientIPVersion: 4, ClientIPSource: "socket", ProtocolNegotiated: "http/1.1"})
	})
	return httptest.NewServer(mux)
}

// Prepare refuses the run rather than letting every stage redial a reaped bus.
func TestPrepareRejectsAPingIntervalPastTheIdleBound(t *testing.T) {
	srv := newWTLatencyServer(t)
	defer srv.Close()
	cfg := DefaultConfig()
	cfg.BaseURL, cfg.LatencyTransport = srv.URL, wire.TransportWebTransport
	cfg.PingInterval = 45 * time.Second
	if _, err := Prepare(t.Context(), cfg); err == nil || !strings.Contains(err.Error(), MaxPingInterval.String()) {
		t.Fatalf("Prepare with a 45s ping interval over the datagram bus = %v, want an error naming the %v bound", err, MaxPingInterval)
	}
}

// The idle bound is the WebTransport bus's own: the server reaps a datagram bus
// it has heard nothing from, and this client's pings are the only traffic on it.
// The WebSocket bus has no idle timer at all, so a wide cadence over it is a
// legal configuration and refusing it names a constraint that cannot apply.
func TestPrepareAcceptsAWideCadenceOverTheWebSocketBus(t *testing.T) {
	srv := newLatencyOnlyServer(t)
	defer srv.Close()
	cfg := DefaultConfig()
	cfg.BaseURL, cfg.LatencyTransport = srv.URL, wire.TransportWebSocket
	cfg.PingInterval = MaxPingInterval + 5*time.Second
	if _, err := Prepare(t.Context(), cfg); err != nil {
		t.Fatalf("Prepare over the WebSocket bus with a %v cadence = %v, want it accepted", cfg.PingInterval, err)
	}
}

// Automatic selection may prefer an advertised datagram bus and then discover
// that UDP cannot reach it. The cadence belongs to the bus Prepare finally
// commits to, so a WebSocket fallback must retain its wider valid cadence.
func TestPrepareAcceptsAWideCadenceAfterWebTransportFallsBack(t *testing.T) {
	t.Parallel()
	mux := http.NewServeMux()
	mux.HandleFunc("/preflight", func(w http.ResponseWriter, r *http.Request) {
		origin := "http://" + r.Host
		ws := testChannel("ws", origin, false)
		wt := testChannel("wt", origin, false)
		wt.Transport, wt.Protocol = wire.TransportWebTransport, "http3"
		_ = json.NewEncoder(w).Encode(wire.Preflight{Capabilities: wire.Capabilities{
			ThroughputTargets: []wire.ThroughputTarget{testTransfer("fetch", origin, "http1", false)},
			LatencyTargets:    []wire.LatencyTarget{ws, wt},
		}})
	})
	mux.HandleFunc("/probe", func(w http.ResponseWriter, _ *http.Request) {
		_ = json.NewEncoder(w).Encode(wire.Probe{ProtocolNegotiated: "http/1.1"})
	})
	mux.Handle("/ws/ping", echoPingHandler())
	srv := httptest.NewServer(mux)
	defer srv.Close()

	cfg := DefaultConfig()
	cfg.BaseURL = srv.URL
	cfg.PingInterval = MaxPingInterval + 5*time.Second
	prepared, err := Prepare(t.Context(), cfg)
	if err != nil {
		t.Fatalf("Prepare after WebTransport fallback: %v", err)
	}
	if got := prepared.LatencyTarget.Transport; got != wire.TransportWebSocket {
		t.Fatalf("latency transport = %q, want WebSocket fallback", got)
	}
}

func TestConfigNormalized(t *testing.T) {
	base := DefaultConfig()

	cases := []struct {
		name   string
		mutate func(c Config) Config
		check  func(c Config) (got, want any)
	}{
		{
			name:   "empty BaseURL defaults",
			mutate: func(c Config) Config { c.BaseURL = ""; return c },
			check:  func(c Config) (any, any) { return c.BaseURL, "http://127.0.0.1:7246" },
		},
		{
			name:   "advertised throughput target id passes through",
			mutate: func(c Config) Config { c.ThroughputTarget = "edge-h2"; return c },
			check:  func(c Config) (any, any) { return c.ThroughputTarget, "edge-h2" },
		},
		{
			name:   "negative Warmup clamps to 0",
			mutate: func(c Config) Config { c.Warmup = -1 * time.Second; return c },
			check:  func(c Config) (any, any) { return c.Warmup, time.Duration(0) },
		},
		{
			name:   "zero LatencyDuration defaults",
			mutate: func(c Config) Config { c.LatencyDuration = 0; return c },
			check:  func(c Config) (any, any) { return c.LatencyDuration, 4 * time.Second },
		},
		{
			name:   "negative LatencyDuration defaults",
			mutate: func(c Config) Config { c.LatencyDuration = -1; return c },
			check:  func(c Config) (any, any) { return c.LatencyDuration, 4 * time.Second },
		},
		{
			name:   "zero DownloadDuration defaults",
			mutate: func(c Config) Config { c.DownloadDuration = 0; return c },
			check:  func(c Config) (any, any) { return c.DownloadDuration, 10 * time.Second },
		},
		{
			name:   "zero UploadDuration defaults",
			mutate: func(c Config) Config { c.UploadDuration = 0; return c },
			check:  func(c Config) (any, any) { return c.UploadDuration, 10 * time.Second },
		},
		{
			name:   "zero BidirectionalDuration defaults",
			mutate: func(c Config) Config { c.BidirectionalDuration = 0; return c },
			check:  func(c Config) (any, any) { return c.BidirectionalDuration, 10 * time.Second },
		},
		{
			name:   "zero automatic max restores default",
			mutate: func(c Config) Config { c.TransferStreams.AutomaticMax = 0; return c },
			check:  func(c Config) (any, any) { return c.TransferStreams.AutomaticMax, 6 },
		},
		{
			name:   "automatic max clamps to 128",
			mutate: func(c Config) Config { c.TransferStreams.AutomaticMax = 500; return c },
			check:  func(c Config) (any, any) { return c.TransferStreams.AutomaticMax, 128 },
		},
		{
			name:   "negative forced stream count selects automatic",
			mutate: func(c Config) Config { c.TransferStreams.Forced = -5; return c },
			check:  func(c Config) (any, any) { return c.TransferStreams.Forced, 0 },
		},
		{
			name:   "forced stream count clamps to 128",
			mutate: func(c Config) Config { c.TransferStreams.Forced = 500; return c },
			check:  func(c Config) (any, any) { return c.TransferStreams.Forced, 128 },
		},
		{
			name:   "forced stream count in range passes through",
			mutate: func(c Config) Config { c.TransferStreams.Forced = 64; return c },
			check:  func(c Config) (any, any) { return c.TransferStreams.Forced, 64 },
		},
		{
			name:   "zero PingInterval defaults",
			mutate: func(c Config) Config { c.PingInterval = 0; return c },
			check:  func(c Config) (any, any) { return c.PingInterval, 250 * time.Millisecond },
		},
		{
			name:   "zero DownloadBytesPerStream defaults",
			mutate: func(c Config) Config { c.DownloadBytesPerStream = 0; return c },
			check:  func(c Config) (any, any) { return c.DownloadBytesPerStream, int64(64 * 1024 * 1024 * 1024) },
		},
		{
			name:   "negative DownloadBytesPerStream defaults",
			mutate: func(c Config) Config { c.DownloadBytesPerStream = -1; return c },
			check:  func(c Config) (any, any) { return c.DownloadBytesPerStream, int64(64 * 1024 * 1024 * 1024) },
		},
		{
			name:   "zero UploadBytesPerStream defaults",
			mutate: func(c Config) Config { c.UploadBytesPerStream = 0; return c },
			check:  func(c Config) (any, any) { return c.UploadBytesPerStream, int64(64 * 1024 * 1024 * 1024) },
		},
		{
			name:   "negative UploadBytesPerStream defaults",
			mutate: func(c Config) Config { c.UploadBytesPerStream = -1; return c },
			check:  func(c Config) (any, any) { return c.UploadBytesPerStream, int64(64 * 1024 * 1024 * 1024) },
		},
		{
			name:   "zero MaxIdleConnsPerHost defaults",
			mutate: func(c Config) Config { c.MaxIdleConnsPerHost = 0; return c },
			check:  func(c Config) (any, any) { return c.MaxIdleConnsPerHost, 256 },
		},
		{
			name:   "zero ResponseHeaderTimeout defaults",
			mutate: func(c Config) Config { c.ResponseHeaderTimeout = 0; return c },
			check:  func(c Config) (any, any) { return c.ResponseHeaderTimeout, 10 * time.Second },
		},
		{
			name:   "zero ExpectContinueTimeout defaults",
			mutate: func(c Config) Config { c.ExpectContinueTimeout = 0; return c },
			check:  func(c Config) (any, any) { return c.ExpectContinueTimeout, time.Second },
		},
	}

	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			c := tc.mutate(base)
			got, want := tc.check(c.normalized())
			if got != want {
				t.Errorf("got %v, want %v", got, want)
			}
		})
	}
}

func TestPreparedConnectionFreshnessAndLabels(t *testing.T) {
	cfg := DefaultConfig()
	prepared := &PreparedConnection{
		ThroughputTarget: wire.ThroughputTarget{Transport: "fetch-stream", Protocol: "http2", TLS: true},
		LatencyTarget:    &wire.LatencyTarget{Transport: "websocket", Protocol: "http1", TLS: false},
		VerifiedAt:       time.Now(),
		configKey:        preparationKey(cfg.normalized()),
	}
	if !prepared.FreshFor(cfg) {
		t.Fatal("fresh matching preparation was rejected")
	}
	if got := prepared.ThroughputSummary(); got != "Fetch stream · HTTP/2 · TLS" {
		t.Fatalf("ThroughputSummary() = %q", got)
	}
	if got := prepared.LatencySummary(); got != "WebSocket · HTTP/1.1 · clear" {
		t.Fatalf("LatencySummary() = %q", got)
	}

	cfg.LatencyTarget = "ws-http1-tls"
	if prepared.FreshFor(cfg) {
		t.Fatal("preparation survived a target change")
	}

	// ValidatePingInterval runs in Prepare only, so a preparation that survived
	// a cadence change would let RunPrepared use one the server's idle bound
	// reaps the bus at.
	cfg = DefaultConfig()
	cfg.PingInterval = MaxPingInterval + time.Second
	if prepared.FreshFor(cfg) {
		t.Fatal("preparation survived a ping-interval change")
	}
	prepared.VerifiedAt = time.Now().Add(-preparationFreshness - time.Second)
	if prepared.FreshFor(DefaultConfig()) {
		t.Fatal("expired preparation was accepted")
	}
}
