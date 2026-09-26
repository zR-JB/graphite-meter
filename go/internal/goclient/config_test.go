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
		{auto, "h2", wire.TransportFetchStream, streamCounts{6, 6}, "Automatic · 1 download / 4 upload"}, // Evidence spellings label; plans use resolved protocols.
		{auto, "http3", wire.TransportWebTransport, streamCounts{1, 1}, "Automatic · 1 continuous stream per direction"},
		{forced, "http2", wire.TransportFetchStream, streamCounts{9, 9}, "Forced · 9 per direction"},
		{forced, "http3", wire.TransportWebTransport, streamCounts{9, 9}, "Forced · 9 per direction"},
		// A forced count still clamps to what one session carries.
		{TransferStreamPolicy{Forced: 99}, "http3", wire.TransportWebTransport, streamCounts{wire.WTMaxStreams, wire.WTMaxStreams}, "Forced · 16 per direction (capped from 99 by the session)"},
	} {
		if got := c.policy.lanes(c.protocol, c.transport); got != c.lanes {
			t.Errorf("%+v lanes(%s, %s) = %+v, want %+v", c.policy, c.protocol, c.transport, got, c.lanes)
		}
		if got := c.policy.Label(c.protocol, c.transport); got != c.label {
			t.Errorf("%+v Label(%s, %s) = %q, want %q", c.policy, c.protocol, c.transport, got, c.label)
		}
	}
}

// Normalization fills only unset or out-of-range values.
func TestConfigNormalizedInvariants(t *testing.T) {
	t.Parallel()
	d := DefaultConfig()
	if got := (Config{}).normalized(); got.BaseURL != d.BaseURL || got.LatencyDuration != d.LatencyDuration || got.PingInterval != d.PingInterval || got.TransferStreams != d.TransferStreams || got.Warmup != 0 {
		t.Fatalf("empty config normalized to %+v", got)
	}
	c := d
	c.ThroughputTarget, c.Warmup, c.DownloadDuration = "edge-h2", -time.Second, -1
	c.TransferStreams = TransferStreamPolicy{AutomaticMax: 500, Forced: -5}
	got := c.normalized()
	if got.ThroughputTarget != "edge-h2" || got.Warmup != 0 || got.DownloadDuration != d.DownloadDuration || got.TransferStreams != (TransferStreamPolicy{AutomaticMax: maxTransferStreams}) {
		t.Fatalf("normalized %+v", got)
	}
	if got := (Config{TransferStreams: TransferStreamPolicy{Forced: 500}}).normalized(); got.TransferStreams.Forced != maxTransferStreams {
		t.Fatalf("forced streams = %d, want the %d ceiling", got.TransferStreams.Forced, maxTransferStreams)
	}
}

func TestValidatePingInterval(t *testing.T) {
	t.Parallel()
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

func newWTLatencyServer(t *testing.T) *httptest.Server {
	t.Helper()
	mux := http.NewServeMux()
	mux.HandleFunc("/preflight", func(w http.ResponseWriter, r *http.Request) {
		origin := "http://" + r.Host
		wtPing := testChannel("wt-ping", origin, false)
		wtPing.Transport, wtPing.Protocol = wire.TransportWebTransport, "http3"
		_ = json.MarshalWrite(w, wire.Preflight{Generation: "test", Capabilities: wire.Capabilities{
			ThroughputTargets: []wire.ThroughputTarget{testTransfer("http1-clear", origin, "http1", false)},
			LatencyTargets:    []wire.LatencyTarget{wtPing},
		}})
	})
	mux.HandleFunc("/probe", func(w http.ResponseWriter, _ *http.Request) {
		_ = json.MarshalWrite(w, wire.Probe{ClientIP: "127.0.0.1", ClientIPVersion: 4, ClientIPSource: "socket", ProtocolNegotiated: "http/1.1"})
	})
	srv := httptest.NewServer(mux)
	t.Cleanup(srv.Close)
	return srv
}

// The WebTransport idle bound applies only when that bus is selected.
func TestPrepareBindsThePingIntervalToTheSelectedBus(t *testing.T) {
	t.Parallel()
	cfg := DefaultConfig()
	cfg.BaseURL, cfg.LatencyTransport = newWTLatencyServer(t).URL, wire.TransportWebTransport
	cfg.PingInterval = 45 * time.Second
	if _, err := prepare(t.Context(), cfg); err == nil || !strings.Contains(err.Error(), MaxPingInterval.String()) {
		t.Fatalf("a 45s interval over the datagram bus = %v, want an error naming the %v bound", err, MaxPingInterval)
	}

	ws := newLatencyOnlyServer(t)
	defer ws.Close()
	cfg.BaseURL, cfg.LatencyTransport = ws.URL, wire.TransportWebSocket
	cfg.PingInterval = MaxPingInterval + 5*time.Second
	if _, err := prepare(t.Context(), cfg); err != nil {
		t.Fatalf("a %v cadence over the WebSocket bus = %v, want it accepted", cfg.PingInterval, err)
	}

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
	mux.HandleFunc("/probe", func(w http.ResponseWriter, _ *http.Request) {
		_ = json.MarshalWrite(w, wire.Probe{ClientIP: "127.0.0.1", ClientIPVersion: 4, ClientIPSource: "socket", ProtocolNegotiated: "http/1.1"})
	})
	mux.Handle("/ws/ping", echoPingHandler())
	fallback := httptest.NewServer(mux)
	defer fallback.Close()
	cfg.BaseURL, cfg.LatencyTransport = fallback.URL, "auto"
	prepared, err := prepare(t.Context(), cfg)
	if err != nil || prepared.LatencyTarget.Transport != wire.TransportWebSocket {
		t.Fatalf("automatic path after an unreachable WebTransport bus = %+v, %v; want the WebSocket fallback", prepared, err)
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
