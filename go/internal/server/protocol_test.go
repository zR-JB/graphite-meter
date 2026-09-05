package server

import (
	"context"
	"crypto/tls"
	"encoding/json/v2"
	"io"
	"net"
	"net/http"
	"strings"
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

func nativeAuthHTTP(t *testing.T, protocol string) (*http.Client, string, *atomic.Int32) {
	t.Helper()
	_, cm := protocolTestTLS(t)
	ln, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	origin := "https://" + ln.Addr().String()
	authn := testPasswordAuth(t, origin)
	var dispatched atomic.Int32
	handler := authn.Enforce(http.HandlerFunc(func(http.ResponseWriter, *http.Request) { dispatched.Add(1) }), auth.Listener{})
	serverProtocols, clientProtocols := &http.Protocols{}, &http.Protocols{}
	var alpn string
	switch protocol {
	case "http1":
		serverProtocols.SetHTTP1(true)
		clientProtocols.SetHTTP1(true)
		alpn = "http/1.1"
	case "http2":
		serverProtocols.SetHTTP2(true)
		clientProtocols.SetHTTP2(true)
		alpn = "h2"
	default:
		t.Fatalf("unsupported native protocol %q", protocol)
	}
	srv := baseServer(handler, serverProtocols)
	go serve(tls.NewListener(ln, cm.tlsConfig(alpn)), srv)
	t.Cleanup(func() { _ = srv.Close(); _ = ln.Close() })
	tr := &http.Transport{TLSClientConfig: &tls.Config{InsecureSkipVerify: true}, Protocols: clientProtocols} //nolint:gosec
	t.Cleanup(tr.CloseIdleConnections)
	return &http.Client{Transport: tr}, origin, &dispatched
}

func nativeAuthHTTP3(t *testing.T) (*http.Client, string, *atomic.Int32) {
	t.Helper()
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
	t.Cleanup(func() { _ = h3.Close(); _ = pc.Close() })
	tr := &http3.Transport{TLSClientConfig: &tls.Config{InsecureSkipVerify: true}, QUICConfig: transport.NewQUICConfig()} //nolint:gosec
	t.Cleanup(func() { _ = tr.Close() })
	return &http.Client{Transport: tr}, origin, &dispatched
}

type zeroReader struct{}

func (*zeroReader) Read([]byte) (int, error) { return 0, io.EOF }

func assertNativeAuthRejects(t *testing.T, client *http.Client, origin string, body io.Reader, dispatched *atomic.Int32) {
	t.Helper()
	req, _ := http.NewRequest(http.MethodPost, origin+"/upload", body)
	res, err := client.Do(req)
	if err != nil {
		t.Fatal(err)
	}
	res.Body.Close()
	if res.StatusCode != http.StatusForbidden || dispatched.Load() != 0 {
		t.Fatalf("status=%d dispatched=%d, want 403 and 0 dispatches", res.StatusCode, dispatched.Load())
	}
}

func TestRealProtocolsRejectBeforeDispatch(t *testing.T) {
	for _, protocol := range []string{"http1", "http2"} {
		t.Run(protocol, func(t *testing.T) {
			client, origin, dispatched := nativeAuthHTTP(t, protocol)
			assertNativeAuthRejects(t, client, origin, io.NopCloser(&zeroReader{}), dispatched)
		})
	}
	t.Run("http3", func(t *testing.T) {
		client, origin, dispatched := nativeAuthHTTP3(t)
		assertNativeAuthRejects(t, client, origin, http.NoBody, dispatched)
	})
}

func TestRealWebSocketHandshakeRejectsBeforeDispatch(t *testing.T) {
	client, origin, dispatched := nativeAuthHTTP(t, "http1")
	ctx, cancel := context.WithTimeout(t.Context(), 5*time.Second)
	defer cancel()
	_, res, err := websocket.Dial(ctx, "wss"+strings.TrimPrefix(origin, "https")+"/ws/ping", &websocket.DialOptions{HTTPClient: client})
	if err == nil {
		t.Fatal("unauthenticated WebSocket handshake succeeded")
	}
	if res == nil || res.StatusCode != http.StatusForbidden || dispatched.Load() != 0 {
		t.Fatalf("response=%v dispatched=%d, want 403 and 0 dispatches", res, dispatched.Load())
	}
	res.Body.Close()
}

func nativeHTTP(t *testing.T, protocol string, topology muxTopology) (*http.Client, string) {
	t.Helper()
	cfg, cm := protocolTestTLS(t)
	ctx := t.Context()
	e, err := buildEndpoints(ctx, cfg)
	if err != nil {
		t.Fatal(err)
	}
	p := &http.Protocols{}
	clientProtocols := &http.Protocols{}
	var alpn string
	switch protocol {
	case "http1":
		p.SetHTTP1(true)
		clientProtocols.SetHTTP1(true)
		alpn = "http/1.1"
	case "http2":
		p.SetHTTP2(true)
		clientProtocols.SetHTTP2(true)
		alpn = "h2"
	default:
		t.Fatalf("unsupported native protocol %q", protocol)
	}
	ln, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	srv := baseServer(listenerMuxConfigured(ctx, e, topology, static.Handler(), nil), p)
	go serve(tls.NewListener(ln, cm.tlsConfig(alpn)), srv)
	t.Cleanup(func() { _ = srv.Close(); _ = ln.Close() })
	tr := &http.Transport{TLSClientConfig: &tls.Config{InsecureSkipVerify: true}, Protocols: clientProtocols} //nolint:gosec
	t.Cleanup(tr.CloseIdleConnections)
	return &http.Client{Transport: tr}, "https://" + ln.Addr().String()
}

func nativeHTTP3(t *testing.T) (*http.Client, string) {
	t.Helper()
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
	t.Cleanup(func() { _ = h3.Close(); _ = pc.Close() })
	tr := &http3.Transport{TLSClientConfig: &tls.Config{InsecureSkipVerify: true}, QUICConfig: transport.NewQUICConfig()} //nolint:gosec
	t.Cleanup(func() { _ = tr.Close() })
	return &http.Client{Transport: tr}, "https://" + pc.LocalAddr().String()
}

func assertProbeAndDownload(t *testing.T, client *http.Client, base, wantProtocol string) {
	t.Helper()
	res, err := client.Get(base + "/probe")
	if err != nil {
		t.Fatal(err)
	}
	var probe wire.Probe
	if err := json.UnmarshalRead(res.Body, &probe); err != nil {
		res.Body.Close()
		t.Fatal(err)
	}
	res.Body.Close()
	if probe.ProtocolNegotiated != wantProtocol {
		t.Fatalf("protocol = %q, want %q", probe.ProtocolNegotiated, wantProtocol)
	}
	res, err = client.Get(base + "/download?bytes=1")
	if err != nil {
		t.Fatal(err)
	}
	body, _ := io.ReadAll(res.Body)
	res.Body.Close()
	if len(body) != 1 {
		t.Fatalf("download bytes = %d, want 1", len(body))
	}
}

func TestNativeHTTP1TLSProbeAndTransfer(t *testing.T) {
	client, base := nativeHTTP(t, "http1", muxTopology{spa: true, discovery: true, latency: true, transfers: true, requiredProto: 1})
	res, err := client.Get(base + "/preflight")
	if err != nil {
		t.Fatal(err)
	}
	res.Body.Close()
	if res.StatusCode != http.StatusOK {
		t.Fatalf("/preflight status = %d, want %d", res.StatusCode, http.StatusOK)
	}
	assertProbeAndDownload(t, client, base, "http/1.1")
}

func TestNativeHTTP2ProbeAndTransfer(t *testing.T) {
	client, base := nativeHTTP(t, "http2", muxTopology{transfers: true, requiredProto: 2})
	for _, path := range []string{"/", "/assets/app.js", "/preflight", "/ws/ping"} {
		res, err := client.Get(base + path)
		if err != nil {
			t.Fatal(err)
		}
		res.Body.Close()
		if res.StatusCode != http.StatusNotFound {
			t.Fatalf("%s status = %d, want 404", path, res.StatusCode)
		}
	}
	assertProbeAndDownload(t, client, base, "h2")
}

func TestNativeHTTP3ProbeAndTransfer(t *testing.T) {
	client, base := nativeHTTP3(t)
	assertProbeAndDownload(t, client, base, "h3")
}
