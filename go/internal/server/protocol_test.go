package server

import (
	"context"
	"crypto/tls"
	"encoding/json/v2"
	"io"
	"net"
	"net/http"
	"sync/atomic"
	"testing"
	"time"

	"github.com/coder/websocket"
	"github.com/quic-go/quic-go/http3"
	"github.com/zR-JB/graphite-meter/go/internal/auth"
	"github.com/zR-JB/graphite-meter/go/internal/config"
	"github.com/zR-JB/graphite-meter/go/internal/static"
	"github.com/zR-JB/graphite-meter/go/internal/transport"
	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

func protocolTestTLS(t *testing.T) (*config.Config, *certificateManager) {
	t.Helper()
	now := time.Now()
	cert, key := writeCertificate(t, t.TempDir(), "server", "meter.example", now.Add(-time.Hour), now.Add(time.Hour))
	cfg := config.Default()
	cfg.TLSCert, cfg.TLSKey = cert, key
	cm, err := newCertificateManager(&cfg)
	if err != nil {
		t.Fatal(err)
	}
	return &cfg, cm
}

func testPasswordAuth(t *testing.T, origin string) *auth.Service {
	t.Helper()
	hash, err := auth.HashPassword("secret")
	if err != nil {
		t.Fatal(err)
	}
	service, err := auth.New(t.Context(), config.AuthConfig{Mode: "password", PublicURL: origin, PasswordHash: hash, OIDCProviderName: "Authelia"}, nil, false)
	if err != nil {
		t.Fatal(err)
	}
	return service
}

func TestRealProtocolsRejectBeforeDispatch(t *testing.T) {
	for _, protocol := range []string{"http1", "http2"} {
		t.Run(protocol, func(t *testing.T) {
			_, cm := protocolTestTLS(t)
			ln, err := net.Listen("tcp", "127.0.0.1:0")
			if err != nil {
				t.Fatal(err)
			}
			origin := "https://" + ln.Addr().String()
			authn := testPasswordAuth(t, origin)
			var dispatched atomic.Int32
			handler := authn.Enforce(http.HandlerFunc(func(http.ResponseWriter, *http.Request) { dispatched.Add(1) }), auth.Listener{})
			serverProtocols := &http.Protocols{}
			clientProtocols := &http.Protocols{}
			if protocol == "http1" {
				serverProtocols.SetHTTP1(true)
				clientProtocols.SetHTTP1(true)
			} else {
				serverProtocols.SetHTTP2(true)
				clientProtocols.SetHTTP2(true)
			}
			srv := baseServer(handler, serverProtocols)
			go serve(tls.NewListener(ln, cm.tlsConfig(map[string]string{"http1": "http/1.1", "http2": "h2"}[protocol])), srv)
			defer srv.Close()
			tr := &http.Transport{TLSClientConfig: &tls.Config{InsecureSkipVerify: true}, Protocols: clientProtocols} //nolint:gosec
			defer tr.CloseIdleConnections()
			req, _ := http.NewRequest(http.MethodPost, origin+"/upload", io.NopCloser(&zeroReader{}))
			res, err := (&http.Client{Transport: tr}).Do(req)
			if err != nil {
				t.Fatal(err)
			}
			res.Body.Close()
			if res.StatusCode != http.StatusForbidden || dispatched.Load() != 0 {
				t.Fatalf("status=%d dispatched=%d, want 403 and 0 dispatches", res.StatusCode, dispatched.Load())
			}
		})
	}

	t.Run("http3", func(t *testing.T) {
		_, cm := protocolTestTLS(t)
		pc, err := net.ListenPacket("udp", "127.0.0.1:0")
		if err != nil {
			t.Fatal(err)
		}
		origin := "https://" + pc.LocalAddr().String()
		authn := testPasswordAuth(t, origin)
		var dispatched atomic.Int32
		h3 := &http3.Server{TLSConfig: cm.tlsConfig(), QUICConfig: transport.NewQUICConfig(), Handler: authn.Enforce(http.HandlerFunc(func(http.ResponseWriter, *http.Request) { dispatched.Add(1) }), auth.Listener{})}
		go h3.Serve(pc)
		defer func() { _ = h3.Close(); _ = pc.Close() }()
		tr := &http3.Transport{TLSClientConfig: &tls.Config{InsecureSkipVerify: true}, QUICConfig: transport.NewQUICConfig()} //nolint:gosec
		defer tr.Close()
		req, _ := http.NewRequest(http.MethodPost, origin+"/upload", http.NoBody)
		res, err := (&http.Client{Transport: tr}).Do(req)
		if err != nil {
			t.Fatal(err)
		}
		res.Body.Close()
		if res.StatusCode != http.StatusForbidden || dispatched.Load() != 0 {
			t.Fatalf("status=%d dispatched=%d", res.StatusCode, dispatched.Load())
		}
	})
}

func TestRealWebSocketHandshakeRejectsBeforeDispatch(t *testing.T) {
	_, cm := protocolTestTLS(t)
	ln, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	origin := "https://" + ln.Addr().String()
	authn := testPasswordAuth(t, origin)
	var dispatched atomic.Int32
	p := &http.Protocols{}
	p.SetHTTP1(true)
	srv := baseServer(authn.Enforce(http.HandlerFunc(func(http.ResponseWriter, *http.Request) { dispatched.Add(1) }), auth.Listener{}), p)
	go serve(tls.NewListener(ln, cm.tlsConfig("http/1.1")), srv)
	defer srv.Close()
	cp := &http.Protocols{}
	cp.SetHTTP1(true)
	tr := &http.Transport{TLSClientConfig: &tls.Config{InsecureSkipVerify: true}, Protocols: cp} //nolint:gosec
	defer tr.CloseIdleConnections()
	ctx, cancel := context.WithTimeout(t.Context(), 5*time.Second)
	defer cancel()
	_, res, err := websocket.Dial(ctx, "wss://"+ln.Addr().String()+"/ws/ping", &websocket.DialOptions{HTTPClient: &http.Client{Transport: tr}})
	if err == nil {
		t.Fatal("unauthenticated WebSocket handshake succeeded")
	}
	if res == nil || res.StatusCode != http.StatusForbidden || dispatched.Load() != 0 {
		t.Fatalf("response=%v dispatched=%d, want 403 and 0 dispatches", res, dispatched.Load())
	}
	res.Body.Close()
}

type zeroReader struct{}

func (*zeroReader) Read([]byte) (int, error) { return 0, io.EOF }

func TestNativeHTTP1TLSProbeAndTransfer(t *testing.T) {
	cfg, cm := protocolTestTLS(t)
	ctx := t.Context()
	e, err := buildEndpoints(ctx, cfg)
	if err != nil {
		t.Fatal(err)
	}
	p := &http.Protocols{}
	p.SetHTTP1(true)
	srv := baseServer(listenerMuxConfigured(ctx, e, muxTopology{spa: true, discovery: true, latency: true, transfers: true, requiredProto: 1}, static.Handler(), nil), p)
	ln, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	go serve(tls.NewListener(ln, cm.tlsConfig("http/1.1")), srv)
	defer srv.Close()
	cp := &http.Protocols{}
	cp.SetHTTP1(true)
	tr := &http.Transport{TLSClientConfig: &tls.Config{InsecureSkipVerify: true}, Protocols: cp} //nolint:gosec
	defer tr.CloseIdleConnections()
	hc := &http.Client{Transport: tr}
	base := "https://" + ln.Addr().String()
	preflight, err := hc.Get(base + "/preflight")
	if err != nil {
		t.Fatal(err)
	}
	preflight.Body.Close()
	if preflight.StatusCode != http.StatusOK {
		t.Fatalf("/preflight status = %d, want %d", preflight.StatusCode, http.StatusOK)
	}
	res, err := hc.Get(base + "/probe")
	if err != nil {
		t.Fatal(err)
	}
	var probe wire.Probe
	if err := json.UnmarshalRead(res.Body, &probe); err != nil {
		t.Fatal(err)
	}
	res.Body.Close()
	if probe.ProtocolNegotiated != "http/1.1" {
		t.Fatalf("protocol = %q, want %q", probe.ProtocolNegotiated, "http/1.1")
	}
	res, err = hc.Get(base + "/download?bytes=1")
	if err != nil {
		t.Fatal(err)
	}
	body, _ := io.ReadAll(res.Body)
	res.Body.Close()
	if len(body) != 1 {
		t.Fatalf("download bytes = %d, want 1", len(body))
	}
}

func TestNativeHTTP2ProbeAndTransfer(t *testing.T) {
	cfg, cm := protocolTestTLS(t)
	ctx := t.Context()
	e, err := buildEndpoints(ctx, cfg)
	if err != nil {
		t.Fatal(err)
	}
	p := &http.Protocols{}
	p.SetHTTP2(true)
	srv := baseServer(listenerMuxConfigured(ctx, e, muxTopology{transfers: true, requiredProto: 2}, static.Handler(), nil), p)
	ln, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	go serve(tls.NewListener(ln, cm.tlsConfig("h2")), srv)
	defer srv.Close()
	cp := &http.Protocols{}
	cp.SetHTTP2(true)
	tr := &http.Transport{TLSClientConfig: &tls.Config{InsecureSkipVerify: true}, Protocols: cp} //nolint:gosec
	defer tr.CloseIdleConnections()
	hc := &http.Client{Transport: tr}
	base := "https://" + ln.Addr().String()
	for _, path := range []string{"/", "/assets/app.js", "/preflight", "/ws/ping"} {
		res, err := hc.Get(base + path)
		if err != nil {
			t.Fatal(err)
		}
		res.Body.Close()
		if res.StatusCode != http.StatusNotFound {
			t.Fatalf("%s status = %d, want 404", path, res.StatusCode)
		}
	}
	res, err := hc.Get(base + "/probe")
	if err != nil {
		t.Fatal(err)
	}
	var probe wire.Probe
	if err := json.UnmarshalRead(res.Body, &probe); err != nil {
		t.Fatal(err)
	}
	res.Body.Close()
	if probe.ProtocolNegotiated != "h2" {
		t.Fatalf("protocol = %q, want %q", probe.ProtocolNegotiated, "h2")
	}
	res, err = hc.Get(base + "/download?bytes=1")
	if err != nil {
		t.Fatal(err)
	}
	body, _ := io.ReadAll(res.Body)
	res.Body.Close()
	if len(body) != 1 {
		t.Fatalf("download bytes = %d, want 1", len(body))
	}
}

func TestNativeHTTP3ProbeAndTransfer(t *testing.T) {
	cfg, cm := protocolTestTLS(t)
	ctx := t.Context()
	e, err := buildEndpoints(ctx, cfg)
	if err != nil {
		t.Fatal(err)
	}
	pc, err := net.ListenPacket("udp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	h3 := &http3.Server{TLSConfig: cm.tlsConfig(), QUICConfig: transport.NewQUICConfig(), Handler: listenerMuxConfigured(ctx, e, muxTopology{transfers: true}, static.Handler(), nil)}
	go h3.Serve(pc)
	defer func() { _ = h3.Close(); _ = pc.Close() }()
	tr := &http3.Transport{TLSClientConfig: &tls.Config{InsecureSkipVerify: true}, QUICConfig: transport.NewQUICConfig()} //nolint:gosec
	defer tr.Close()
	hc := &http.Client{Transport: tr}
	base := "https://" + pc.LocalAddr().String()
	res, err := hc.Get(base + "/probe")
	if err != nil {
		t.Fatal(err)
	}
	var probe wire.Probe
	if err := json.UnmarshalRead(res.Body, &probe); err != nil {
		t.Fatal(err)
	}
	res.Body.Close()
	if probe.ProtocolNegotiated != "h3" {
		t.Fatalf("protocol = %q, want %q", probe.ProtocolNegotiated, "h3")
	}
	res, err = hc.Get(base + "/download?bytes=1")
	if err != nil {
		t.Fatal(err)
	}
	body, _ := io.ReadAll(res.Body)
	res.Body.Close()
	if len(body) != 1 {
		t.Fatalf("download bytes = %d, want 1", len(body))
	}
}

func TestHTTP3StartsAtMinimumPacketSize(t *testing.T) {
	cfg := transport.NewQUICConfig()
	if cfg.InitialPacketSize != 1200 {
		t.Fatalf("initial packet size = %d, want 1200", cfg.InitialPacketSize)
	}
	if cfg.DisablePathMTUDiscovery {
		t.Fatal("path MTU discovery is disabled")
	}
}
