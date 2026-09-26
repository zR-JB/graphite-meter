package goclient

import (
	"context"
	"errors"
	"io"
	"net/http"
	"net/http/httptest"
	"net/url"
	"slices"
	"strings"
	"sync/atomic"
	"testing"
	"time"

	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

func TestAuthenticationLoginURLStaysOnTheServerHostname(t *testing.T) {
	t.Parallel()
	base, _ := url.Parse("https://meter.example:7248")
	if login, err := authenticationLoginURL(
		base,
		"https://meter.example:7247/login",
	); err != nil || login.Host != "meter.example:7247" {
		t.Fatalf("login on another port = %v, %v", login, err)
	}
	for _, raw := range []string{
		"https://login.example:7247/login",
		"http://meter.example/login",
		"https://meter.example/login?next=x",
		"https://meter.example/other",
	} {
		if _, err := authenticationLoginURL(base, raw); err == nil {
			t.Fatalf("accepted authentication URL %s", raw)
		}
	}
}

func okResponse(r *http.Request) (*http.Response, error) {
	return &http.Response{
		StatusCode: http.StatusOK,
		Body:       io.NopCloser(strings.NewReader("")),
		Header:     http.Header{},
		Request:    r,
	}, nil
}

func TestAuthenticatedClientAddsBearerOnlyOnAdvertisedHTTPSOrigins(t *testing.T) {
	t.Parallel()
	cred := credential{token: "secret", origins: []string{"https://meter.example"}}
	var pf wire.Preflight
	pf.Capabilities.ThroughputTargets = []wire.ThroughputTarget{
		{Origin: "https://meter.example:7247"},
		{Origin: "https://cdn.example"},
		{Origin: "http://meter.example:8080"},
	}
	cred.reach("https://meter.example", pf)
	seen := ""
	client := authenticatedClient(cred, roundTripFunc(func(r *http.Request) (*http.Response, error) {
		seen = r.Header.Get("Authorization")
		return okResponse(r)
	}))
	for _, target := range []string{"https://meter.example/probe", "https://meter.example:7247/probe"} {
		seen = ""
		req, _ := http.NewRequestWithContext(t.Context(), "GET", target, nil)
		if _, err := client.Do(req); err != nil || seen != "Bearer secret" {
			t.Fatalf("%s: authorization=%q, %v", target, seen, err)
		}
	}
	for _, origin := range []string{
		"https://meter.example:9443",
		"https://other.example",
		"https://cdn.example",
		"https://evil-meter.example",
		"https://meter.example.evil.example",
		"http://meter.example",
		"http://meter.example:8080",
	} {
		bad, _ := http.NewRequest("GET", origin+"/probe", nil)
		if _, err := client.Do(bad); err == nil {
			t.Fatalf("grant sent to %s outside the server's advertised HTTPS origins", origin)
		}
		if _, err := wtDial(t.Context(), cred, origin, "/wt/ping", nil); err == nil ||
			!strings.Contains(err.Error(), "refusing") {
			t.Fatalf("WebTransport grant toward %s: %v", origin, err)
		}
	}
}

func TestAuthenticatedClientNeverFollowsRedirects(t *testing.T) {
	t.Parallel()
	for _, location := range []string{
		"https://meter.example:9443/download",
		"https://other.example/download",
		"http://meter.example/download",
	} {
		cred := credential{token: "secret", origins: []string{"https://meter.example"}}
		calls := 0
		client := authenticatedClient(cred, roundTripFunc(func(r *http.Request) (*http.Response, error) {
			calls++
			return &http.Response{
				StatusCode: http.StatusTemporaryRedirect,
				Body:       io.NopCloser(strings.NewReader("")),
				Header:     http.Header{"Location": {location}},
				Request:    r,
			}, nil
		}))
		req, _ := http.NewRequest(http.MethodGet, "https://meter.example/download", nil)
		if _, err := client.Do(req); err == nil || calls != 1 {
			t.Fatalf("redirect to %s: err=%v after %d requests", location, err, calls)
		}
	}
}

func TestAuthenticatedOperationRejectsInsecureMode(t *testing.T) {
	t.Parallel()
	cfg := DefaultConfig()
	cfg.BaseURL = "https://meter.example"
	cfg.InsecureSkipTLSVerify = true
	if _, err := beginAuthorization(cfg, "https://meter.example/login"); err == nil {
		t.Fatal("authenticated -insecure accepted")
	}
}

func TestGrantNeverCrossesUnverifiedTLS(t *testing.T) {
	t.Parallel()
	var presented atomic.Bool
	srv := httptest.NewTLSServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		presented.Store(presented.Load() || r.Header.Get("Authorization") != "")
		http.NotFound(w, r)
	}))
	defer srv.Close()
	cfg := DefaultConfig()
	cfg.BaseURL, cfg.InsecureSkipTLSVerify = srv.URL, true
	if _, err := prepareRun(t.Context(), cfg, nil, map[string]string{srv.URL: "secret"}); err == nil {
		t.Fatal("prepared an authenticated run without TLS verification")
	}
	if presented.Load() {
		t.Fatal("grant crossed a connection whose certificate was not verified")
	}
	cred := credential{token: "secret", origins: []string{srv.URL}, insecure: true}
	if _, err := wtDial(t.Context(), cred, srv.URL, "/wt/ping", nil); err == nil ||
		!strings.Contains(err.Error(), "refusing") {
		t.Fatalf("WebTransport dial with a grant and -insecure: %v", err)
	}
}

func pendingApproval(transport roundTripFunc) *PendingAuthorization {
	return &PendingAuthorization{
		verifier: "verifier",
		tokenURL: "https://meter.example/auth/cli/token",
		close:    func() {},
		client:   &http.Client{Transport: transport},
	}
}

func TestPollNamesWhyApprovalEnded(t *testing.T) {
	t.Parallel()
	pending := func(r *http.Request) (*http.Response, error) {
		return &http.Response{
			StatusCode: http.StatusAccepted,
			Body:       io.NopCloser(strings.NewReader(`{"status":"pending"}`)),
			Header:     http.Header{},
			Request:    r,
		}, nil
	}
	refused := func(*http.Request) (*http.Response, error) { return nil, errors.New("connection refused") }
	for _, c := range []struct {
		transport roundTripFunc
		want      string
	}{
		{refused, "connection refused"},
		{pending, "browser approval timed out"},
	} {
		ctx, cancel := context.WithTimeout(t.Context(), 20*time.Millisecond)
		_, err := pendingApproval(c.transport).Poll(ctx)
		cancel()
		if err == nil || !strings.Contains(err.Error(), c.want) {
			t.Fatalf("err=%v, want %q", err, c.want)
		}
	}
	ctx, cancel := context.WithCancel(t.Context())
	cancel()
	if _, err := pendingApproval(pending).Poll(ctx); !errors.Is(err, context.Canceled) {
		t.Fatalf("err=%v, want context.Canceled", err)
	}
}

func TestPollRejectsMalformedSuccessfulApproval(t *testing.T) {
	t.Parallel()
	for _, body := range []string{
		`{"token":"ok"} {}`,
		`{"token":""}`,
		`{"token":"` + strings.Repeat("a", 8193) + `"}`,
		strings.Repeat(" ", maxControlBytes+1),
	} {
		approval := pendingApproval(func(r *http.Request) (*http.Response, error) {
			return &http.Response{
				StatusCode: http.StatusOK,
				Body:       io.NopCloser(strings.NewReader(body)),
				Header:     http.Header{},
				Request:    r,
			}, nil
		})
		if _, err := approval.Poll(t.Context()); err == nil {
			t.Fatal("accepted malformed approval")
		}
	}
}

// A redirected approval poll would re-send the verifier wherever the redirect points.
func TestApprovalPollNeverFollowsARedirect(t *testing.T) {
	t.Parallel()
	cfg := DefaultConfig()
	cfg.BaseURL = "https://meter.example"
	pending, err := beginAuthorization(cfg, "https://meter.example/login")
	if err != nil {
		t.Fatal(err)
	}
	var hosts []string
	pending.client.Transport = roundTripFunc(func(r *http.Request) (*http.Response, error) {
		hosts = append(hosts, r.URL.Host)
		return &http.Response{
			StatusCode: http.StatusTemporaryRedirect,
			Body:       io.NopCloser(strings.NewReader("")),
			Header:     http.Header{"Location": {"https://collector.example/auth/cli/token"}},
			Request:    r,
		}, nil
	})
	ctx, cancel := context.WithTimeout(t.Context(), 50*time.Millisecond)
	defer cancel()
	if _, err := pending.Poll(ctx); err == nil {
		t.Fatal("a redirected poll returned a grant")
	}
	if !slices.Equal(hosts, []string{"meter.example"}) {
		t.Fatalf("poll requests reached %v, want only the issuer", hosts)
	}
}

func TestAuthenticatedPreparationRequiresVerifiedHTTPS(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		base     string
		insecure bool
	}{{"http://127.0.0.1:1", false}, {"https://127.0.0.1:1", true}} {
		cfg := DefaultConfig()
		cfg.BaseURL, cfg.InsecureSkipTLSVerify = tc.base, tc.insecure
		cred := credential{token: "secret", origins: []string{tc.base}, insecure: tc.insecure}
		_, err := prepare(t.Context(), cfg, nil, &cred)
		if err == nil || !strings.Contains(err.Error(), "verified HTTPS") {
			t.Fatalf("prepare %s insecure=%t with a grant: %v", tc.base, tc.insecure, err)
		}
	}
}

func TestAcceptAuthorizationKeysGrantsByCanonicalOrigin(t *testing.T) {
	t.Parallel()
	c := NewController(t.Context())
	defer c.Close()
	for _, origin := range []string{"https://Meter.example", "https://meter.example:443", "https://meter.example/",
		"meter.example"} {
		if err := c.AcceptAuthorization(origin, "grant"); err == nil {
			t.Errorf("accepted a grant for the non-canonical origin %q", origin)
		}
	}
	if err := c.AcceptAuthorization("https://meter.example", "grant"); err != nil {
		t.Fatal(err)
	}
	if grants := c.snapshot(); len(grants) != 1 || grants["https://meter.example"] != "grant" {
		t.Fatalf("grants = %v, want one under the canonical origin", grants)
	}
}
