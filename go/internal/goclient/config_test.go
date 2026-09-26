package goclient

import (
	"encoding/json/v2"
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
		lanes               streamCounts
		label               string
	}{
		{auto, "http1", wire.TransportFetchStream, streamCounts{6, 6}, "Automatic · up to 6 per direction"},
		{auto, "http2", wire.TransportFetchStream, streamCounts{1, 4}, "Automatic · 1 download / 4 upload"},
		{auto, "http3", wire.TransportFetchStream, streamCounts{1, 1}, "Automatic · 1 download / 1 upload"},
		{
			auto,
			"h2",
			wire.TransportFetchStream,
			streamCounts{6, 6},
			"Automatic · 1 download / 4 upload",
		},
		{
			auto,
			"http3",
			wire.TransportWebTransport,
			streamCounts{1, 1},
			"Automatic · 1 continuous stream per direction",
		},
		{forced, "http2", wire.TransportFetchStream, streamCounts{9, 9}, "Forced · 9 per direction"},
		{forced, "http3", wire.TransportWebTransport, streamCounts{9, 9}, "Forced · 9 per direction"},
		{
			TransferStreamPolicy{Forced: 99},
			"http3",
			wire.TransportWebTransport,
			streamCounts{wire.WTMaxStreams, wire.WTMaxStreams},
			"Forced · 16 per direction (capped from 99 by the session)",
		},
	} {
		if got := c.policy.lanes(c.protocol, c.transport); got != c.lanes {
			t.Errorf("%+v lanes(%s, %s) = %+v, want %+v", c.policy, c.protocol, c.transport, got, c.lanes)
		}
		if got := c.policy.Label(c.protocol, c.transport); got != c.label {
			t.Errorf("%+v Label(%s, %s) = %q, want %q", c.policy, c.protocol, c.transport, got, c.label)
		}
	}
}

func TestConfigNormalizedInvariants(t *testing.T) {
	t.Parallel()
	d := DefaultConfig()
	if got := (Config{}).normalized(); got.BaseURL != d.BaseURL ||
		got.LatencyDuration != d.LatencyDuration ||
		got.PingInterval != PingMedium || got.LoadedPingInterval != d.LoadedPingInterval ||
		got.TransferStreams != d.TransferStreams ||
		got.Warmup != 0 {
		t.Fatalf("empty config normalized to %+v", got)
	}
	c := d
	c.ThroughputTarget, c.Warmup, c.DownloadDuration = "edge-h2", -time.Second, -1
	c.TransferStreams = TransferStreamPolicy{AutomaticMax: 500, Forced: -5}
	got := c.normalized()
	if got.ThroughputTarget != "edge-h2" ||
		got.Warmup != 0 ||
		got.DownloadDuration != d.DownloadDuration || got.TransferStreams != (TransferStreamPolicy{
		AutomaticMax: MaxTransferStreams,
	}) {
		t.Fatalf("normalized %+v", got)
	}
	if got := (Config{
		TransferStreams: TransferStreamPolicy{Forced: 500},
	}).normalized(); got.TransferStreams.Forced != MaxTransferStreams {
		t.Fatalf("forced streams = %d, want the %d ceiling", got.TransferStreams.Forced, MaxTransferStreams)
	}
}

func TestConfigValidate(t *testing.T) {
	t.Parallel()
	if MaxPingInterval*2 != wire.WTIdleBound {
		t.Errorf("MaxPingInterval = %v, want half of the %v idle bound", MaxPingInterval, wire.WTIdleBound)
	}
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
		{"WebSocket unbounded", func(c *Config) {
			c.LatencyTransport, c.PingInterval = wire.TransportWebSocket, 45*time.Second
		}, ""},
	} {
		cfg := DefaultConfig()
		// An unreachable base URL proves prepare validates before discovery.
		cfg.BaseURL = "https://127.0.0.1:1"
		c.edit(&cfg)
		err := cfg.Validate()
		if c.want == "" && err != nil || c.want != "" && (err == nil || !strings.Contains(err.Error(), c.want)) {
			t.Errorf("%s: Validate = %v, want %q", c.name, err, c.want)
		}
		if _, prepareErr := prepare(t.Context(), cfg); c.want != "" && prepareErr.Error() != err.Error() {
			t.Errorf("%s: prepare = %v, want the validation error", c.name, prepareErr)
		}
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
			ThroughputTargets: []wire.ThroughputTarget{testTransfer("fetch", origin, "http1", false)},
			LatencyTargets:    []wire.LatencyTarget{testChannel("ws", origin, false), wt},
		}})
	})
	mux.HandleFunc("/probe", writeProbe)
	mux.Handle("/ws/ping", pingHandler(answerAll, 0))
	srv := httptest.NewServer(mux)
	defer srv.Close()
	cfg := DefaultConfig()
	cfg.BaseURL, cfg.PingInterval = srv.URL, MaxPingInterval+5*time.Second
	prepared, err := prepare(t.Context(), cfg)
	if err != nil || prepared.LatencyTarget.Transport != wire.TransportWebSocket {
		t.Fatalf("automatic path after an unreachable WebTransport bus = %+v, %v; want WebSocket", prepared, err)
	}
}

func TestPreparedConnectionFreshness(t *testing.T) {
	t.Parallel()
	cfg := DefaultConfig()
	prepared := &PreparedConnection{VerifiedAt: time.Now(), configKey: preparationKey(cfg.normalized())}
	if !prepared.FreshFor(cfg) {
		t.Fatal("fresh matching preparation was rejected")
	}
	changed := cfg
	changed.LatencyTarget = "ws-http1-tls"
	if prepared.FreshFor(changed) {
		t.Fatal("preparation survived a target change")
	}
	changed = cfg
	changed.PingInterval = MaxPingInterval + time.Second
	if prepared.FreshFor(changed) {
		t.Fatal("preparation survived a ping-interval change")
	}
	prepared.VerifiedAt = time.Now().Add(-preparationFreshness - time.Second)
	if prepared.FreshFor(cfg) {
		t.Fatal("expired preparation was accepted")
	}
}
