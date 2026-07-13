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
	"github.com/zR-JB/graphite-meter/go/internal/endpoint"
	"github.com/zR-JB/graphite-meter/go/internal/transport"
)

// stubEndpoint is a minimal endpoint.Endpoint that writes a fixed body, used
// to tell "the registered endpoint ran" apart from "the static handler ran"
// without depending on any real endpoint's behavior.
type stubEndpoint struct{ body string }

func (e stubEndpoint) ID() string                          { return "stub" }
func (e stubEndpoint) Capabilities() endpoint.Capabilities { return endpoint.Capabilities{HTTP: true} }
func (e stubEndpoint) Handle(s transport.Session) error {
	w, _, _ := s.HTTP()
	_, err := io.WriteString(w, e.body)
	return err
}

func TestBuildMuxMountsRegisteredEndpoint(t *testing.T) {
	reg := endpoint.NewRegistry()
	reg.RegisterHTTP("/fake", stubEndpoint{body: "fake-endpoint-response"})

	mux := BuildMux(context.Background(), reg)

	rec := httptest.NewRecorder()
	req := httptest.NewRequest(http.MethodGet, "/fake", nil)
	mux.ServeHTTP(rec, req)

	if rec.Code != http.StatusOK {
		t.Fatalf("status = %d, want 200", rec.Code)
	}
	if got := rec.Body.String(); got != "fake-endpoint-response" {
		t.Fatalf("body = %q, want the registered endpoint's response", got)
	}
}

func TestBuildMuxFallsThroughToStaticAtRoot(t *testing.T) {
	reg := endpoint.NewRegistry()
	reg.RegisterHTTP("/fake", stubEndpoint{body: "fake-endpoint-response"})

	mux := BuildMux(context.Background(), reg)

	rec := httptest.NewRecorder()
	req := httptest.NewRequest(http.MethodGet, "/", nil)
	mux.ServeHTTP(rec, req)

	// Any path other than a registered one must reach the static handler, not
	// the registered endpoint — regardless of what the static handler itself
	// decides to serve for "/" (that logic is embed_test.go's job).
	if got := rec.Body.String(); got == "fake-endpoint-response" {
		t.Fatalf("body = %q, want the static handler's response, not the endpoint's", got)
	}
}

func TestH3BootstrapCannotServeTransfers(t *testing.T) {
	cfg := config.Default()
	e, err := buildEndpoints(context.Background(), &cfg)
	if err != nil {
		t.Fatal(err)
	}
	mux := bootstrapMux(e)
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
	mux := fullMux(context.Background(), e, muxTopology{discovery: true, requiredProto: 2})
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

func TestH1MountsLatencyAndH3MountsProgress(t *testing.T) {
	cfg := config.Default()
	e, err := buildEndpoints(context.Background(), &cfg)
	if err != nil {
		t.Fatal(err)
	}
	h1 := fullMux(context.Background(), e, muxTopology{discovery: true, latency: true, requiredProto: 1})
	rec := httptest.NewRecorder()
	h1.ServeHTTP(rec, httptest.NewRequest(http.MethodGet, "/ws/ping", nil))
	if rec.Code == http.StatusNotFound {
		t.Fatal("H1 latency websocket is not mounted")
	}
	h3 := h3Mux(e)
	rec = httptest.NewRecorder()
	h3.ServeHTTP(rec, httptest.NewRequest(http.MethodGet, "/upload/progress?id=unknown", nil))
	if rec.Code == http.StatusNotFound {
		t.Fatal("H3 upload progress is not mounted")
	}
}

func TestPublicH3Port(t *testing.T) {
	cfg := config.Default()
	if got := publicH3Port(&cfg); got != "8444" {
		t.Fatalf("default port = %q", got)
	}
	cfg.PublicH3Origin = "https://meter.example:18444"
	if got := publicH3Port(&cfg); got != "18444" {
		t.Fatalf("public port = %q", got)
	}
	cfg.PublicH3Origin = "https://meter.example"
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
