package server

import (
	"context"
	"io"
	"net/http"
	"net/http/httptest"
	"testing"

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
