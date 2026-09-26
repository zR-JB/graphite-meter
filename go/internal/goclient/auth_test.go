package goclient

import (
	"context"
	"errors"
	"io"
	"net/http"
	"net/http/httptest"
	"net/url"
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
	cfg := DefaultConfig()
	cfg.BaseURL, cfg.grant = "https://meter.example", "secret"
	var pf wire.Preflight
	pf.Capabilities.ThroughputTargets = []wire.ThroughputTarget{
		{Origin: "https://meter.example:7247"},
		{Origin: "https://cdn.example"},
	}
	cfg.grantOrigins = grantOrigins(cfg.BaseURL, pf)
	seen := ""
	client := authenticatedClient(cfg, roundTripFunc(func(r *http.Request) (*http.Response, error) {
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
	for _, target := range []string{
		"https://meter.example:9443/probe",
		"https://other.example/probe",
		"https://cdn.example/probe",
		"http://meter.example/probe",
	} {
		bad, _ := http.NewRequest("GET", target, nil)
		if _, err := client.Do(bad); err == nil {
			t.Fatalf("grant sent to %s outside the server's advertised HTTPS origins", target)
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
		cfg := DefaultConfig()
		cfg.BaseURL, cfg.grant = "https://meter.example", "secret"
		calls := 0
		client := authenticatedClient(cfg, roundTripFunc(func(r *http.Request) (*http.Response, error) {
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
	cfg.grant = "secret"
	if _, err := wtDial(t.Context(), cfg, srv.URL, "/wt/ping", nil); err == nil ||
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
