package server

import (
	"bytes"
	"context"
	"crypto/tls"
	"errors"
	"io"
	"net"
	"net/http"
	"net/http/httptest"
	"testing"
	"time"

	"github.com/zR-JB/graphite-meter/go/internal/auth"
	"github.com/zR-JB/graphite-meter/go/internal/config"
)

type observedBody struct {
	reader *bytes.Reader
	read   int
}

func (b *observedBody) Read(p []byte) (int, error) {
	n, err := b.reader.Read(p)
	b.read += n
	return n, err
}

func TestH3BootstrapCannotServeTransfers(t *testing.T) {
	cfg := config.Default()
	e, err := buildEndpoints(context.Background(), &cfg)
	if err != nil {
		t.Fatal(err)
	}
	mux := listenerMux(context.Background(), e, muxTopology{bootstrap: true})
	for _, path := range []string{"/download", "/upload", "/upload/session", "/upload/progress", "/ws/ping"} {
		rec := httptest.NewRecorder()
		mux.ServeHTTP(rec, httptest.NewRequest(http.MethodGet, path, nil))
		if rec.Code != http.StatusNotFound {
			t.Errorf("%s status = %d", path, rec.Code)
		}
	}
}

func TestH2ThroughputRoutesRequireHTTP2(t *testing.T) {
	cfg := config.Default()
	e, err := buildEndpoints(context.Background(), &cfg)
	if err != nil {
		t.Fatal(err)
	}
	mux := listenerMux(context.Background(), e, muxTopology{transfers: true, requiredProto: 2})
	rec := httptest.NewRecorder()
	req := httptest.NewRequest(http.MethodGet, "/download?bytes=1", nil)
	mux.ServeHTTP(rec, req)
	if rec.Code != http.StatusNotFound {
		t.Fatalf("h1 transfer status = %d", rec.Code)
	}
	rec = httptest.NewRecorder()
	mux.ServeHTTP(rec, httptest.NewRequest(http.MethodGet, "/ws/ping", nil))
	if rec.Code != http.StatusNotFound {
		t.Fatalf("H2 websocket status = %d", rec.Code)
	}
}

func TestH2MountsOnlyMeasurementHTTPRoutes(t *testing.T) {
	cfg := config.Default()
	e, err := buildEndpoints(context.Background(), &cfg)
	if err != nil {
		t.Fatal(err)
	}
	mux := listenerMux(context.Background(), e, muxTopology{transfers: true, requiredProto: 2})
	for _, path := range []string{"/", "/assets/app.js", "/preflight", "/ws/ping"} {
		rec := httptest.NewRecorder()
		req := httptest.NewRequest(http.MethodGet, path, nil)
		req.Proto, req.ProtoMajor, req.ProtoMinor = "HTTP/2.0", 2, 0
		mux.ServeHTTP(rec, req)
		if rec.Code != http.StatusNotFound {
			t.Errorf("%s status = %d, want 404", path, rec.Code)
		}
	}
	for _, path := range []string{"/probe", "/download?bytes=1", "/upload/session", "/upload", "/upload/progress"} {
		rec := httptest.NewRecorder()
		req := httptest.NewRequest(http.MethodGet, path, nil)
		req.Proto, req.ProtoMajor, req.ProtoMinor = "HTTP/2.0", 2, 0
		mux.ServeHTTP(rec, req)
		if rec.Code == http.StatusNotFound && path != "/upload/progress" {
			t.Errorf("%s is not mounted", path)
		}
	}
}

func TestH1MountsSPAAndDiscovery(t *testing.T) {
	cfg := config.Default()
	e, err := buildEndpoints(context.Background(), &cfg)
	if err != nil {
		t.Fatal(err)
	}
	mux := listenerMuxWithSPA(context.Background(), e, muxTopology{spa: true, discovery: true, latency: true, transfers: true}, http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		w.WriteHeader(http.StatusOK)
	}))
	for _, path := range []string{"/", "/preflight"} {
		rec := httptest.NewRecorder()
		mux.ServeHTTP(rec, httptest.NewRequest(http.MethodGet, path, nil))
		if rec.Code != http.StatusOK {
			t.Errorf("%s status = %d, want 200", path, rec.Code)
		}
	}
}

func TestAuthenticationWrapsEveryFinalListenerBeforeDispatch(t *testing.T) {
	hash, err := auth.HashPassword("secret")
	if err != nil {
		t.Fatal(err)
	}
	authn, err := auth.New(context.Background(), config.AuthConfig{Mode: "password", PublicURL: "https://meter.example", PasswordHash: hash, OIDCProviderName: "Authelia"}, nil, false)
	if err != nil {
		t.Fatal(err)
	}
	cfg := config.Default()
	e, err := buildEndpoints(context.Background(), &cfg)
	if err != nil {
		t.Fatal(err)
	}
	tests := []struct {
		name     string
		topology muxTopology
		listener auth.Listener
		path     string
		proto    int
	}{
		{"h1-ui", muxTopology{spa: true, discovery: true, latency: true, transfers: true}, auth.Listener{UI: true}, "/", 1},
		{"h1-static", muxTopology{spa: true, discovery: true, latency: true, transfers: true}, auth.Listener{UI: true}, "/asset.js", 1},
		{"h1-upload", muxTopology{spa: true, discovery: true, latency: true, transfers: true}, auth.Listener{UI: true}, "/upload", 1},
		{"h2", muxTopology{transfers: true, requiredProto: 2}, auth.Listener{}, "/download", 2},
		{"h3-bootstrap", muxTopology{bootstrap: true}, auth.Listener{}, "/probe", 1},
		{"h3", muxTopology{transfers: true}, auth.Listener{}, "/upload", 3},
		{"websocket", muxTopology{latency: true}, auth.Listener{}, "/ws/ping", 1},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			body := &observedBody{reader: bytes.NewReader(bytes.Repeat([]byte("x"), 1024))}
			mux := listenerMuxConfigured(context.Background(), e, test.topology, http.HandlerFunc(func(http.ResponseWriter, *http.Request) { t.Fatal("SPA dispatched") }), authn)
			handler := authn.Enforce(mux, test.listener)
			req := httptest.NewRequest(http.MethodPost, "https://meter.example"+test.path, body)
			req.Host = "meter.example"
			req.TLS = &tls.ConnectionState{}
			req.ProtoMajor = test.proto
			recorder := httptest.NewRecorder()
			handler.ServeHTTP(recorder, req)
			if recorder.Code != http.StatusForbidden || body.read != 0 {
				t.Fatalf("status=%d body-read=%d", recorder.Code, body.read)
			}
		})
	}
	if stats := e.admission.stats(); stats.active != 0 || stats.peak != 0 {
		t.Fatalf("unauthenticated requests reached admission: %+v", stats)
	}
}

func TestH1MountsLatencyAndH3MountsProgress(t *testing.T) {
	cfg := config.Default()
	e, err := buildEndpoints(context.Background(), &cfg)
	if err != nil {
		t.Fatal(err)
	}
	h1 := listenerMux(context.Background(), e, muxTopology{discovery: true, latency: true, transfers: true, requiredProto: 1})
	rec := httptest.NewRecorder()
	h1.ServeHTTP(rec, httptest.NewRequest(http.MethodGet, "/ws/ping", nil))
	if rec.Code == http.StatusNotFound {
		t.Fatal("H1 latency websocket is not mounted")
	}
	h3 := listenerMux(context.Background(), e, muxTopology{transfers: true})
	rec = httptest.NewRecorder()
	h3.ServeHTTP(rec, httptest.NewRequest(http.MethodGet, "/upload/progress?id=unknown", nil))
	if rec.Code == http.StatusNotFound {
		t.Fatal("H3 upload progress is not mounted")
	}
}

func TestPublicH3Port(t *testing.T) {
	cfg := config.Default()
	cfg.Native.H3 = ":7249"
	if got := publicH3Port(&cfg); got != "7249" {
		t.Fatalf("default port = %q", got)
	}
	cfg.NativePublic.H3 = "https://meter.example:18444"
	if got := publicH3Port(&cfg); got != "18444" {
		t.Fatalf("public port = %q", got)
	}
	cfg.NativePublic.H3 = "https://meter.example"
	if got := publicH3Port(&cfg); got != "443" {
		t.Fatalf("default TLS port = %q", got)
	}
}

func TestServeHandlesRequestsOverAnExplicitListener(t *testing.T) {
	ln, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatalf("net.Listen: %v", err)
	}

	mux := http.NewServeMux()
	mux.HandleFunc("/ping", func(w http.ResponseWriter, r *http.Request) {
		io.WriteString(w, "pong")
	})
	srv := &http.Server{Handler: mux}

	done := make(chan error, 1)
	go func() { done <- serve(ln, srv) }()

	resp, err := http.Get("http://" + ln.Addr().String() + "/ping")
	if err != nil {
		t.Fatalf("GET /ping: %v", err)
	}
	body, _ := io.ReadAll(resp.Body)
	resp.Body.Close()
	if string(body) != "pong" {
		t.Fatalf("body = %q, want %q", body, "pong")
	}

	if err := ln.Close(); err != nil {
		t.Fatalf("close listener: %v", err)
	}

	select {
	case err := <-done:
		if err == nil || !errors.Is(err, net.ErrClosed) {
			t.Fatalf("serve returned %v, want a closed-listener error", err)
		}
	case <-time.After(2 * time.Second):
		t.Fatal("serve did not return after the listener closed")
	}
}
