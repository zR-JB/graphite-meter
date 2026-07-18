package server

import (
	"context"
	"errors"
	"io"
	"net"
	"net/http"
	"net/http/httptest"
	"testing"
	"time"

	"github.com/zR-JB/graphite-meter/go/internal/config"
)

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
