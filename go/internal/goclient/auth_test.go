package goclient

import (
	"context"
	"errors"
	"io"
	"net/http"
	"net/url"
	"strings"
	"testing"
	"time"
)

func TestAuthenticationLoginURLAcceptsCanonicalHostnameOnAnotherPort(t *testing.T) {
	base, _ := url.Parse("https://meter.example:7248")
	login, err := authenticationLoginURL(base, "https://meter.example:7247/login")
	if err != nil {
		t.Fatal(err)
	}
	if login.Host != "meter.example:7247" {
		t.Fatalf("Host = %q", login.Host)
	}
}

func TestAuthenticationLoginURLRejectsDifferentHostname(t *testing.T) {
	base, _ := url.Parse("https://meter.example:7248")
	if _, err := authenticationLoginURL(base, "https://login.example:7247/login"); err == nil {
		t.Fatal("accepted authentication URL on another hostname")
	}
}

type roundTripFunc func(*http.Request) (*http.Response, error)

func (f roundTripFunc) RoundTrip(r *http.Request) (*http.Response, error) { return f(r) }

func TestAuthenticatedClientAddsBearerOnlyOnCanonicalHTTPSHost(t *testing.T) {
	cfg := DefaultConfig()
	cfg.BaseURL = "https://meter.example"
	cfg.AuthToken = "secret"
	cfg.AuthOrigin = "https://meter.example"
	seen := ""
	client := authenticatedClient(cfg, roundTripFunc(func(r *http.Request) (*http.Response, error) {
		seen = r.Header.Get("Authorization")
		return &http.Response{StatusCode: 200, Body: io.NopCloser(strings.NewReader("")), Header: http.Header{}, Request: r}, nil
	}))
	req, _ := http.NewRequestWithContext(context.Background(), "GET", "https://meter.example:7247/probe", nil)
	if _, err := client.Do(req); err != nil {
		t.Fatal(err)
	}
	if seen != "Bearer secret" {
		t.Fatalf("authorization=%q", seen)
	}
	bad, _ := http.NewRequest("GET", "https://other.example/probe", nil)
	if _, err := client.Do(bad); err == nil {
		t.Fatal("grant sent outside canonical host")
	}
}

func TestAuthenticatedClientDoesNotSendBearerAfterServerChange(t *testing.T) {
	cfg := DefaultConfig()
	cfg.BaseURL = "https://other.example"
	cfg.AuthToken = "secret"
	cfg.AuthOrigin = "https://meter.example"
	seen := ""
	client := authenticatedClient(cfg, roundTripFunc(func(r *http.Request) (*http.Response, error) {
		seen = r.Header.Get("Authorization")
		return &http.Response{StatusCode: 200, Body: io.NopCloser(strings.NewReader("")), Header: http.Header{}, Request: r}, nil
	}))
	req, _ := http.NewRequestWithContext(context.Background(), "GET", "https://other.example/preflight", nil)
	if _, err := client.Do(req); err != nil {
		t.Fatal(err)
	}
	if seen != "" {
		t.Fatalf("authorization leaked after server change: %q", seen)
	}
}

func TestAuthenticatedClientNeverFollowsRedirects(t *testing.T) {
	for _, location := range []string{
		"https://meter.example:9443/download",
		"https://other.example/download",
		"http://meter.example/download",
	} {
		t.Run(location, func(t *testing.T) {
			cfg := DefaultConfig()
			cfg.BaseURL = "https://meter.example"
			cfg.AuthToken = "secret"
			cfg.AuthOrigin = "https://meter.example"
			calls := 0
			client := authenticatedClient(cfg, roundTripFunc(func(r *http.Request) (*http.Response, error) {
				calls++
				header := make(http.Header)
				header.Set("Location", location)
				return &http.Response{StatusCode: http.StatusTemporaryRedirect, Body: io.NopCloser(strings.NewReader("")), Header: header, Request: r}, nil
			}))
			req, _ := http.NewRequest(http.MethodGet, "https://meter.example/download", nil)
			if _, err := client.Do(req); err == nil {
				t.Fatal("authenticated redirect was accepted")
			}
			if calls != 1 {
				t.Fatalf("redirect caused %d requests", calls)
			}
		})
	}
}

func TestAuthenticatedOperationRejectsInsecureMode(t *testing.T) {
	cfg := DefaultConfig()
	cfg.BaseURL = "https://meter.example"
	cfg.InsecureSkipTLSVerify = true
	if _, err := BeginAuthorization(cfg, "https://meter.example/login"); err == nil {
		t.Fatal("authenticated -insecure accepted")
	}
}

func TestCanonicalServerOriginRejectsNonOrigins(t *testing.T) {
	for _, raw := range []string{"https://user@meter.example", "https://meter.example/path", "https://meter.example?query=1", "https://meter.example?", "https://meter.example#fragment", "ftp://meter.example"} {
		t.Run(raw, func(t *testing.T) {
			if _, err := CanonicalServerOrigin(raw); err == nil {
				t.Fatal("non-origin server URL accepted")
			}
		})
	}
	for _, raw := range []string{"https://meter.example", "https://meter.example/"} {
		if got, err := CanonicalServerOrigin(raw); err != nil || got != "https://meter.example" {
			t.Fatalf("CanonicalServerOrigin(%q) = %q, %v", raw, got, err)
		}
	}
}

func TestClassifyAuthFailureDetectsRevokedGrant(t *testing.T) {
	runErr := errors.New("stream closed")
	client := &http.Client{Transport: roundTripFunc(func(r *http.Request) (*http.Response, error) {
		header := make(http.Header)
		header.Set("Graphite-Meter-Auth", "required")
		header.Set("Graphite-Meter-Auth-URL", "https://meter.example/login")
		return &http.Response{StatusCode: http.StatusForbidden, Body: io.NopCloser(strings.NewReader("")), Header: header, Request: r}, nil
	})}
	err := classifyAuthFailure(context.Background(), client, "https://meter.example", runErr)
	var authErr *AuthRequiredError
	if !errors.As(err, &authErr) || authErr.URL != "https://meter.example/login" {
		t.Fatalf("error=%v", err)
	}
}

// An AuthOrigin that does not parse must leave the pinned hostname empty
// rather than fail the client — which, with no grant to send, means an
// ordinary unauthenticated request.
func TestAuthenticatedClientSurvivesUnparseableOrigin(t *testing.T) {
	cfg := DefaultConfig()
	cfg.BaseURL = "https://meter.example"
	cfg.AuthToken = "secret"
	cfg.AuthOrigin = "https://meter.example\x7f:bad"
	seen := "unset"
	client := authenticatedClient(cfg, roundTripFunc(func(r *http.Request) (*http.Response, error) {
		seen = r.Header.Get("Authorization")
		return &http.Response{StatusCode: 200, Body: io.NopCloser(strings.NewReader("")), Header: http.Header{}, Request: r}, nil
	}))
	req, _ := http.NewRequest(http.MethodGet, "https://meter.example/probe", nil)
	if _, err := client.Do(req); err != nil {
		t.Fatal(err)
	}
	if seen != "" {
		t.Fatalf("authorization=%q, want no grant", seen)
	}
}

// Poll retries transport errors, but a deadline must report why the wait
// failed rather than a bare "context deadline exceeded".
func TestPollSurfacesLastTransportErrorOnDeadline(t *testing.T) {
	p := &PendingAuthorization{
		verifier: "verifier", tokenURL: "https://meter.example/auth/cli/token", close: func() {},
		client: &http.Client{Transport: roundTripFunc(func(*http.Request) (*http.Response, error) {
			return nil, errors.New("connection refused")
		})},
	}
	ctx, cancel := context.WithTimeout(context.Background(), 20*time.Millisecond)
	defer cancel()
	_, err := p.Poll(ctx)
	if err == nil || !strings.Contains(err.Error(), "connection refused") {
		t.Fatalf("err=%v, want the retained transport error", err)
	}
}

func TestPollReportsTimeoutWhenTheServerKeptAnswering(t *testing.T) {
	p := &PendingAuthorization{
		verifier: "verifier", tokenURL: "https://meter.example/auth/cli/token", close: func() {},
		client: &http.Client{Transport: roundTripFunc(func(r *http.Request) (*http.Response, error) {
			return &http.Response{StatusCode: http.StatusAccepted, Body: io.NopCloser(strings.NewReader(`{"status":"pending"}`)), Header: http.Header{}, Request: r}, nil
		})},
	}
	ctx, cancel := context.WithTimeout(context.Background(), 20*time.Millisecond)
	defer cancel()
	_, err := p.Poll(ctx)
	if err == nil || !strings.Contains(err.Error(), "browser approval timed out") {
		t.Fatalf("err=%v, want a timeout the operator can act on", err)
	}
}

func TestPollPropagatesCancellation(t *testing.T) {
	p := &PendingAuthorization{
		verifier: "verifier", tokenURL: "https://meter.example/auth/cli/token", close: func() {},
		client: &http.Client{Transport: roundTripFunc(func(r *http.Request) (*http.Response, error) {
			return &http.Response{StatusCode: http.StatusAccepted, Body: io.NopCloser(strings.NewReader(`{"status":"pending"}`)), Header: http.Header{}, Request: r}, nil
		})},
	}
	ctx, cancel := context.WithCancel(context.Background())
	cancel()
	if _, err := p.Poll(ctx); !errors.Is(err, context.Canceled) {
		t.Fatalf("err=%v, want context.Canceled", err)
	}
}
