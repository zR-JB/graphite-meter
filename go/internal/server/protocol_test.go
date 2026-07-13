package server

import (
	"context"
	"crypto/tls"
	"encoding/json"
	"io"
	"net"
	"net/http"
	"testing"
	"time"

	"github.com/quic-go/quic-go"
	"github.com/quic-go/quic-go/http3"
	webtransport "github.com/quic-go/webtransport-go"
	"github.com/zR-JB/graphite-meter/go/internal/config"
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

func TestNativeHTTP2ProbeAndTransfer(t *testing.T) {
	cfg, cm := protocolTestTLS(t)
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	e, err := buildEndpoints(ctx, cfg)
	if err != nil {
		t.Fatal(err)
	}
	p := &http.Protocols{}
	p.SetHTTP1(true)
	p.SetHTTP2(true)
	srv := baseServer(fullMux(ctx, e, false, 2, true), p)
	ln, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	go serve(tls.NewListener(ln, cm.tlsConfig("h2", "http/1.1")), srv)
	defer srv.Close()
	cp := &http.Protocols{}
	cp.SetHTTP2(true)
	tr := &http.Transport{TLSClientConfig: &tls.Config{InsecureSkipVerify: true}, Protocols: cp} //nolint:gosec
	defer tr.CloseIdleConnections()
	hc := &http.Client{Transport: tr}
	base := "https://" + ln.Addr().String()
	res, err := hc.Get(base + "/probe")
	if err != nil {
		t.Fatal(err)
	}
	var probe wire.Probe
	if err := json.NewDecoder(res.Body).Decode(&probe); err != nil {
		t.Fatal(err)
	}
	res.Body.Close()
	if probe.ProtocolNegotiated != "h2" {
		t.Fatalf("protocol = %q", probe.ProtocolNegotiated)
	}
	res, err = hc.Get(base + "/download?bytes=1")
	if err != nil {
		t.Fatal(err)
	}
	body, _ := io.ReadAll(res.Body)
	res.Body.Close()
	if len(body) != 1 {
		t.Fatalf("download bytes = %d", len(body))
	}
}

func TestNativeHTTP3ProbeAndTransfer(t *testing.T) {
	cfg, cm := protocolTestTLS(t)
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	e, err := buildEndpoints(ctx, cfg)
	if err != nil {
		t.Fatal(err)
	}
	pc, err := net.ListenPacket("udp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	h3 := &http3.Server{TLSConfig: cm.tlsConfig(), QUICConfig: &quic.Config{Allow0RTT: false}, Handler: h3Mux(e)}
	webtransport.ConfigureHTTP3Server(h3)
	go h3.Serve(pc)
	defer func() { _ = h3.Close(); _ = pc.Close() }()
	tr := &http3.Transport{TLSClientConfig: &tls.Config{InsecureSkipVerify: true}} //nolint:gosec
	defer tr.Close()
	hc := &http.Client{Transport: tr}
	base := "https://" + pc.LocalAddr().String()
	res, err := hc.Get(base + "/probe")
	if err != nil {
		t.Fatal(err)
	}
	var probe wire.Probe
	if err := json.NewDecoder(res.Body).Decode(&probe); err != nil {
		t.Fatal(err)
	}
	res.Body.Close()
	if probe.ProtocolNegotiated != "h3" {
		t.Fatalf("protocol = %q", probe.ProtocolNegotiated)
	}
	res, err = hc.Get(base + "/download?bytes=1")
	if err != nil {
		t.Fatal(err)
	}
	body, _ := io.ReadAll(res.Body)
	res.Body.Close()
	if len(body) != 1 {
		t.Fatalf("download bytes = %d", len(body))
	}
}
