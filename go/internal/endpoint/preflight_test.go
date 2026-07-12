package endpoint

import (
	"crypto/tls"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"net/netip"
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
	cfg.TrustedProxies = []netip.Prefix{netip.MustParsePrefix("192.0.2.0/24")}
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

// TestPreflightOriginsBaseline checks the no-config, no-proxy baseline: h1 is
// derived from the request Host, and tls/h3 are both omitted.
func TestPreflightOriginsBaseline(t *testing.T) {
	cfg := config.Default()
	pf := NewPreflight(&cfg)

	req := httptest.NewRequest(http.MethodGet, "http://speed.example:8765/preflight", nil)
	req.Host = "speed.example:8765"
	body := pf.build(&fakeSession{}, req)

	if got, want := *body.Capabilities.Origins.H1, "http://speed.example:8765"; got != want {
		t.Errorf("h1 origin = %q, want %q", got, want)
	}
	if body.Capabilities.Origins.TLS != nil {
		t.Errorf("tls origin = %q, want nil with no config and no proxy header", *body.Capabilities.Origins.TLS)
	}
	if body.Capabilities.Origins.H3 != nil {
		t.Errorf("h3 origin = %q, want nil without PUBLIC_H3_ORIGIN", *body.Capabilities.Origins.H3)
	}
}

// TestPreflightOriginsH3WhenConfigured checks PublicH3Origin, unexercised by
// the other origin tests, is surfaced verbatim.
func TestPreflightOriginsH3WhenConfigured(t *testing.T) {
	cfg := config.Default()
	cfg.PublicH3Origin = "https://speed.example:443"
	pf := NewPreflight(&cfg)

	req := httptest.NewRequest(http.MethodGet, "http://speed.example:8765/preflight", nil)
	req.Host = "speed.example:8765"
	body := pf.build(&fakeSession{}, req)

	if body.Capabilities.Origins.H3 == nil {
		t.Fatal("h3 origin = nil, want the configured PUBLIC_H3_ORIGIN")
	}
	if got, want := *body.Capabilities.Origins.H3, cfg.PublicH3Origin; got != want {
		t.Errorf("h3 origin = %q, want %q", got, want)
	}
}

// TestPreflightCapabilitiesTransportFlags locks the advertised transport
// flags: fetch-stream and WebSocket are live, WebTransport is not yet mounted.
func TestPreflightCapabilitiesTransportFlags(t *testing.T) {
	cfg := config.Default()
	pf := NewPreflight(&cfg)
	req := httptest.NewRequest(http.MethodGet, "http://speed.example/preflight", nil)
	body := pf.build(&fakeSession{}, req)

	got := body.Capabilities.Transports
	want := wire.Transports{FetchStream: true, WebSocket: true, WebTransport: false}
	if got != want {
		t.Errorf("Transports = %+v, want %+v", got, want)
	}
}

func TestPreflightClientAddress(t *testing.T) {
	cfg := config.Default()
	cfg.TrustedProxies = []netip.Prefix{netip.MustParsePrefix("10.0.0.0/8")}
	req := httptest.NewRequest(http.MethodGet, "http://speed.example/preflight", nil)
	req.RemoteAddr = "10.0.0.2:1234"
	req.Header.Set("Forwarded", `for="[2001:db8::4]:443"`)

	body := NewPreflight(&cfg).build(&fakeSession{}, req)
	if body.ClientIP != "2001:db8::4" || body.ClientIPVersion != 6 || body.ClientIPSource != "forwarded" {
		t.Fatalf("client address = %s/IPv%d/%s", body.ClientIP, body.ClientIPVersion, body.ClientIPSource)
	}
}

// TestRequestIsTLSDirectConnection checks r.TLS set (a direct TLS
// connection, no proxy involved) is honored independently of any header.
func TestRequestIsTLSDirectConnection(t *testing.T) {
	req := httptest.NewRequest(http.MethodGet, "https://speed.example/preflight", nil)
	req.TLS = &tls.ConnectionState{}
	if !requestIsTLS(req, nil) {
		t.Error("requestIsTLS = false with r.TLS set, want true")
	}
}

// TestRequestIsTLSMultiHopTakesFirstEntry checks that a comma-separated
// X-Forwarded-Proto (a chain of proxies) is read only at its first entry —
// documented as "only the first hop is read" — regardless of what later
// hops in the chain say.
func TestRequestIsTLSMultiHopTakesFirstEntry(t *testing.T) {
	cases := []struct {
		name  string
		proto string
		want  bool
	}{
		{"first-hop-https", "https, http", true},
		{"first-hop-http", "http, https", false},
		{"single-https-with-space", " https ", true},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			req := httptest.NewRequest(http.MethodGet, "http://speed.example/preflight", nil)
			req.Header.Set("X-Forwarded-Proto", tc.proto)
			trusted := []netip.Prefix{netip.MustParsePrefix("192.0.2.0/24")}
			if got := requestIsTLS(req, trusted); got != tc.want {
				t.Errorf("requestIsTLS(X-Forwarded-Proto=%q) = %v, want %v", tc.proto, got, tc.want)
			}
		})
	}
}

func TestRequestIsTLSIgnoresUntrustedHeader(t *testing.T) {
	req := httptest.NewRequest(http.MethodGet, "http://speed.example/preflight", nil)
	req.Header.Set("X-Forwarded-Proto", "https")
	if requestIsTLS(req, nil) {
		t.Error("requestIsTLS = true for an untrusted forwarding header")
	}
}

// TestHostPortDefaultsWhenNoPort checks a bare hostname (no ":port") in Host
// defaults to port 80 rather than erroring or leaving it zero.
func TestHostPortDefaultsWhenNoPort(t *testing.T) {
	req := httptest.NewRequest(http.MethodGet, "http://speed.example/preflight", nil)
	req.Host = "speed.example"
	host, port := hostPort(req)
	if host != "speed.example" || port != 80 {
		t.Errorf("hostPort(%q) = (%q, %d), want (%q, 80)", req.Host, host, port, "speed.example")
	}
}

// TestHostPortDefaultsOnMalformedPort checks a non-numeric port component
// also defaults to 80 rather than propagating the parse error.
func TestHostPortDefaultsOnMalformedPort(t *testing.T) {
	req := httptest.NewRequest(http.MethodGet, "http://speed.example/preflight", nil)
	req.Host = "speed.example:notaport"
	host, port := hostPort(req)
	if host != "speed.example" || port != 80 {
		t.Errorf("hostPort(%q) = (%q, %d), want (%q, 80)", req.Host, host, port, "speed.example")
	}
}
