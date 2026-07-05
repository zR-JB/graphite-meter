package endpoint

import (
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"testing"

	"github.com/zR-JB/graphite-meter/go/internal/config"
	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

// TestPreflightAdvertisesUploadSession checks that /preflight advertises the
// upload warmup token endpoint without minting a token globally.
func TestPreflightAdvertisesUploadSession(t *testing.T) {
	cfg := config.Default()
	mux := http.NewServeMux()
	mux.Handle("/preflight", httpAdapter(NewPreflight(&cfg)))
	srv := httptest.NewServer(mux)
	defer srv.Close()

	res, err := http.Get(srv.URL + "/preflight")
	if err != nil {
		t.Fatalf("get: %v", err)
	}
	defer res.Body.Close()
	var raw map[string]any
	if err := json.NewDecoder(res.Body).Decode(&raw); err != nil {
		t.Fatalf("decode raw: %v", err)
	}
	if _, present := raw["uploadId"]; present {
		t.Errorf("uploadId present in preflight, want omitted")
	}
	var pf wire.Preflight
	body, _ := json.Marshal(raw)
	if err := json.Unmarshal(body, &pf); err != nil {
		t.Fatalf("decode struct: %v", err)
	}
	if pf.Capabilities.Endpoints.UploadSession != "/upload/session" {
		t.Errorf("uploadSession = %q, want /upload/session", pf.Capabilities.Endpoints.UploadSession)
	}
	if pf.Capabilities.Endpoints.WSUpload != "/ws/upload" {
		t.Errorf("wsUpload = %q, want /ws/upload", pf.Capabilities.Endpoints.WSUpload)
	}
}

// TestPreflightOriginsBehindTLSProxy checks that a request forwarded from a
// TLS-terminating reverse proxy gets a populated `tls` origin, so the client
// has a real wss(-mappable) origin to prefer over the always-http `h1`.
func TestPreflightOriginsBehindTLSProxy(t *testing.T) {
	cfg := config.Default()
	pf := NewPreflight(&cfg)

	req := httptest.NewRequest(http.MethodGet, "http://speed.example:8765/preflight", nil)
	req.Host = "speed.example:8765"
	body := pf.build(&fakeSession{}, req)
	if body.Capabilities.Origins.TLS != nil {
		t.Fatalf("tls origin = %v, want nil without X-Forwarded-Proto", *body.Capabilities.Origins.TLS)
	}

	req.Header.Set("X-Forwarded-Proto", "https")
	body = pf.build(&fakeSession{}, req)
	if body.Capabilities.Origins.TLS == nil {
		t.Fatal("tls origin = nil, want derived https origin with X-Forwarded-Proto: https")
	}
	if want := "https://speed.example:8765"; *body.Capabilities.Origins.TLS != want {
		t.Errorf("tls origin = %q, want %q", *body.Capabilities.Origins.TLS, want)
	}
	// h1 stays cleartext regardless — the two fields are independent.
	if *body.Capabilities.Origins.H1 != "http://speed.example:8765" {
		t.Errorf("h1 origin = %q, want unchanged http://", *body.Capabilities.Origins.H1)
	}
}

// TestPreflightOriginsPublicTLSOriginWins checks that an explicit
// PUBLIC_TLS_ORIGIN override always wins over the X-Forwarded-Proto
// derivation.
func TestPreflightOriginsPublicTLSOriginWins(t *testing.T) {
	cfg := config.Default()
	cfg.PublicTLSOrigin = "https://configured.example:9443"
	pf := NewPreflight(&cfg)

	req := httptest.NewRequest(http.MethodGet, "http://speed.example:8765/preflight", nil)
	req.Host = "speed.example:8765"
	req.Header.Set("X-Forwarded-Proto", "https")
	body := pf.build(&fakeSession{}, req)
	if got, want := *body.Capabilities.Origins.TLS, cfg.PublicTLSOrigin; got != want {
		t.Errorf("tls origin = %q, want configured override %q", got, want)
	}
}
