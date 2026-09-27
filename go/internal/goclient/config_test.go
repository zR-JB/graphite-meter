package goclient

import (
	"encoding/json/v2"
	"errors"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
	"time"

	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

func TestTransferStreamPolicy(t *testing.T) {
	t.Parallel()
	auto := TransferStreamPolicy{AutomaticMax: 6}
	forced := TransferStreamPolicy{Forced: 9}
	for _, c := range []struct {
		policy              TransferStreamPolicy
		protocol, transport string
		down, up            int
	}{
		{auto, "http1", wire.TransportFetchStream, 6, 6},
		{auto, "http2", wire.TransportFetchStream, 1, 4},
		{auto, "http3", wire.TransportFetchStream, 1, 1},
		{auto, "http3", wire.TransportWebTransport, 1, 1},
		{forced, "http2", wire.TransportFetchStream, 9, 9},
		{forced, "http3", wire.TransportWebTransport, 9, 9},
	} {
		if down, up := c.policy.Lanes(c.protocol, c.transport); down != c.down || up != c.up {
			t.Errorf("%+v Lanes(%s, %s) = %d/%d, want %d/%d", c.policy, c.protocol, c.transport, down, up, c.down, c.up)
		}
	}
}

func TestConfigNormalizedClamps(t *testing.T) {
	t.Parallel()
	d := DefaultConfig()
	c := d
	c.ThroughputTarget, c.Warmup, c.DownloadDuration = "edge-h2", -time.Second, -1
	c.TransferStreams = TransferStreamPolicy{AutomaticMax: 500, Forced: -5}
	clamped := TransferStreamPolicy{AutomaticMax: MaxStreams}
	if got := c.normalized(); got.ThroughputTarget != "edge-h2" || got.Warmup != 0 ||
		got.DownloadDuration != d.DownloadDuration || got.TransferStreams != clamped {
		t.Fatalf("normalized %+v", got)
	}
	c.TransferStreams = TransferStreamPolicy{Forced: 500}
	if got := c.normalized(); got.TransferStreams.Forced != MaxStreams {
		t.Fatalf("forced streams = %d, want the %d ceiling", got.TransferStreams.Forced, MaxStreams)
	}
}

func TestConfigValidate(t *testing.T) {
	t.Parallel()
	for _, c := range []struct {
		name string
		edit func(*Config)
		want string
	}{
		{"defaults", func(*Config) {}, ""},
		{"protocol", func(c *Config) { c.ThroughputProtocol = "spdy" }, "invalid throughput protocol"},
		{"throughput transport", func(c *Config) { c.ThroughputTransport = "webscoket" }, "throughput transport"},
		{"datagrams", func(c *Config) { c.ThroughputTransport = "webtransport-datagram" }, "throughput transport"},
		{"latency transport", func(c *Config) { c.LatencyTransport = "webtransport-datagram" }, "latency transport"},
		{"WebTransport bound", func(c *Config) {
			c.LatencyTransport, c.PingInterval = wire.TransportWebTransport, MaxPingInterval+time.Millisecond
		}, MaxPingInterval.String()},
		{"WebTransport at bound", func(c *Config) {
			c.LatencyTransport, c.PingInterval = wire.TransportWebTransport, MaxPingInterval
		}, ""},
		{"WebSocket bound", func(c *Config) {
			c.LatencyTransport, c.PingInterval = wire.TransportWebSocket, 45*time.Second
		}, MaxPingInterval.String()},
		{"no stage", func(c *Config) { c.Stages = StageSet{} }, "select at least one stage"},
		{"warmup", func(c *Config) { c.Warmup = -time.Second }, "warmup must be from 0 s to 4 s"},
		{"short stage", func(c *Config) { c.DownloadDuration = 999 * time.Millisecond }, "download duration must be"},
		{"long stage", func(c *Config) { c.BidirectionalDuration = time.Hour }, "bidirectional duration must be"},
		{"fast ping", func(c *Config) { c.LoadedPingInterval = 79 * time.Millisecond }, "at least 80ms"},
		{"streams", func(c *Config) { c.TransferStreams.Forced = MaxStreams + 1 }, "streams must be"},
	} {
		cfg := DefaultConfig()
		// An unreachable base URL proves prepare validates before discovery.
		cfg.BaseURL = "https://127.0.0.1:1"
		c.edit(&cfg)
		err := cfg.Validate()
		if c.want == "" && err != nil || c.want != "" && (err == nil || !strings.Contains(err.Error(), c.want)) {
			t.Errorf("%s: Validate = %v, want %q", c.name, err, c.want)
		}
		pathErr := cfg.normalized().checkPaths()
		if _, prepareErr := prepareOne(t.Context(), cfg); pathErr != nil && prepareErr.Error() != pathErr.Error() {
			t.Errorf("%s: prepare = %v, want the validation error", c.name, prepareErr)
		}
	}
}

func TestPrepareRefusesUploadsWithoutReceiverCheckpoints(t *testing.T) {
	t.Parallel()
	mux := http.NewServeMux()
	mux.HandleFunc("/preflight", func(w http.ResponseWriter, r *http.Request) {
		origin := "http://" + r.Host
		_ = json.MarshalWrite(w, wire.Preflight{Generation: "test", Capabilities: wire.Capabilities{
			ThroughputTargets: []wire.ThroughputTarget{testTransfer("fetch", origin, "http1", false)},
			LatencyTargets:    []wire.LatencyTarget{testChannel("ws", origin, false)},
		}})
	})
	mux.HandleFunc("/probe", writeProbe)
	mux.Handle("/ws/ping", pingHandler(answerAll, 0))
	srv := httptest.NewServer(mux)
	defer srv.Close()
	cfg := DefaultConfig()
	cfg.BaseURL = srv.URL
	_, err := prepareOne(t.Context(), cfg)
	if failed, ok := errors.AsType[*PreparationError](err); !ok || failed.Preflight.Generation != "test" {
		t.Fatalf("upload stage without receiver checkpoints = %v, want a refusal that keeps discovery", err)
	}
	cfg.Stages = StageSet{Latency: true, Download: true}
	if _, err := prepareOne(t.Context(), cfg); err != nil {
		t.Fatalf("download-only run refused: %v", err)
	}
}

func TestPrepareFallsBackFromAnUnreachableWebTransportBus(t *testing.T) {
	t.Parallel()
	mux := http.NewServeMux()
	mux.HandleFunc("/preflight", func(w http.ResponseWriter, r *http.Request) {
		origin := "http://" + r.Host
		wt := testChannel("wt", origin, false)
		wt.Transport, wt.Protocol = wire.TransportWebTransport, "http3"
		_ = json.MarshalWrite(w, wire.Preflight{Generation: "test", Capabilities: wire.Capabilities{
			UploadCheckpoint:  true,
			ThroughputTargets: []wire.ThroughputTarget{testTransfer("fetch", origin, "http1", false)},
			LatencyTargets:    []wire.LatencyTarget{testChannel("ws", origin, false), wt},
		}})
	})
	mux.HandleFunc("/probe", writeProbe)
	mux.Handle("/ws/ping", pingHandler(answerAll, 0))
	srv := httptest.NewServer(mux)
	defer srv.Close()
	cfg := DefaultConfig()
	cfg.BaseURL = srv.URL
	prepared, err := prepareOne(t.Context(), cfg)
	if err != nil || prepared.LatencyTarget.Transport != wire.TransportWebSocket {
		t.Fatalf("automatic path after an unreachable WebTransport bus = %+v, %v; want WebSocket", prepared, err)
	}
}

func TestPreparedRunFreshness(t *testing.T) {
	t.Parallel()
	cfg := DefaultConfig()
	cfg.ServerIDs, cfg.Stages = []string{"b", "a"}, StageSet{Latency: true, Download: true}
	prepared := &PreparedRun{VerifiedAt: time.Now(), key: cfg.PreparationKey(), Servers: []PreparedServer{
		{Connection: &PreparedConnection{}}}}
	for _, c := range []struct {
		name  string
		edit  func(*Config)
		fresh bool
	}{
		{"unchanged", func(*Config) {}, true},
		{"latency target", func(c *Config) { c.LatencyTarget = "ws-http1-tls" }, false},
		{"latency interval", func(c *Config) { c.PingInterval = MaxPingInterval + time.Second }, false},
		{"settings preparation does not depend on", func(c *Config) {
			c.Stages.Latency, c.DownloadDuration, c.ServerIDs = false, time.Minute, []string{"a", "b"}
		}, true},
		{"upload stage without receiver checkpoints", func(c *Config) { c.Stages.Bidirectional = true }, false},
	} {
		changed := cfg
		c.edit(&changed)
		if prepared.FreshFor(changed) != c.fresh {
			t.Errorf("%s: fresh = %v, want %v", c.name, !c.fresh, c.fresh)
		}
	}
	prepared.VerifiedAt = time.Now().Add(-PreparationFreshness - time.Second)
	if prepared.FreshFor(cfg) {
		t.Fatal("expired preparation was accepted")
	}
}
