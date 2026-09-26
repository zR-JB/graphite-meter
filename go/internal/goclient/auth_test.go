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

	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

func TestAuthenticationLoginURLStaysOnTheServerHostname(t *testing.T) {
	t.Parallel()
	base, _ := url.Parse("https://meter.example:7248")
	if login, err := authenticationLoginURL(base, "https://meter.example:7247/login"); err != nil || login.Host != "meter.example:7247" {
		t.Fatalf("login on another port = %v, %v", login, err)
	}
	for _, raw := range []string{"https://login.example:7247/login", "http://meter.example/login", "https://meter.example/login?next=x", "https://meter.example/other"} {
		if _, err := authenticationLoginURL(base, raw); err == nil {
			t.Fatalf("accepted authentication URL %s", raw)
		}
	}
}

type roundTripFunc func(*http.Request) (*http.Response, error)

func (f roundTripFunc) RoundTrip(r *http.Request) (*http.Response, error) { return f(r) }

func okResponse(r *http.Request) (*http.Response, error) {
	return &http.Response{StatusCode: http.StatusOK, Body: io.NopCloser(strings.NewReader("")), Header: http.Header{}, Request: r}, nil
}

// A grant reaches only its issuer's HTTPS hostname, never additional origins.
func TestAuthenticatedClientAddsBearerOnlyOnTheIssuerHTTPSHostname(t *testing.T) {
	t.Parallel()
	cfg := DefaultConfig()
	cfg.BaseURL, cfg.grant = "https://meter.example", "secret"
	cfg.server = &wire.ServerEntry{ID: "a", URL: "https://meter.example", AdditionalOrigins: []string{"https://cdn.example"}}
	seen := ""
	client := authenticatedClient(cfg, roundTripFunc(func(r *http.Request) (*http.Response, error) {
		seen = r.Header.Get("Authorization")
		return okResponse(r)
	}))
	req, _ := http.NewRequestWithContext(t.Context(), "GET", "https://meter.example:7247/probe", nil)
	if _, err := client.Do(req); err != nil || seen != "Bearer secret" {
		t.Fatalf("authorization=%q, %v", seen, err)
	}
	for _, target := range []string{"https://other.example/probe", "https://cdn.example/probe", "http://meter.example/probe"} {
		bad, _ := http.NewRequest("GET", target, nil)
		if _, err := client.Do(bad); err == nil {
			t.Fatalf("grant sent to %s outside its issuer's HTTPS hostname", target)
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
			return &http.Response{StatusCode: http.StatusTemporaryRedirect, Body: io.NopCloser(strings.NewReader("")), Header: http.Header{"Location": {location}}, Request: r}, nil
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

func pendingApproval(transport roundTripFunc) *PendingAuthorization {
	return &PendingAuthorization{verifier: "verifier", tokenURL: "https://meter.example/auth/cli/token", close: func() {}, client: &http.Client{Transport: transport}}
}

func TestPollNamesWhyApprovalEnded(t *testing.T) {
	t.Parallel()
	pending := func(r *http.Request) (*http.Response, error) {
		return &http.Response{StatusCode: http.StatusAccepted, Body: io.NopCloser(strings.NewReader(`{"status":"pending"}`)), Header: http.Header{}, Request: r}, nil
	}
	refused := func(*http.Request) (*http.Response, error) { return nil, errors.New("connection refused") }
	for _, c := range []struct {
		transport roundTripFunc
		want      string
	}{
		// A deadline names the last transport error.
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
	for _, body := range []string{`{"token":"ok"} {}`, `{"token":""}`, `{"token":"` + strings.Repeat("a", 8193) + `"}`, strings.Repeat(" ", maxControlBytes+1)} {
		approval := pendingApproval(func(r *http.Request) (*http.Response, error) {
			return &http.Response{StatusCode: http.StatusOK, Body: io.NopCloser(strings.NewReader(body)), Header: http.Header{}, Request: r}, nil
		})
		if _, err := approval.Poll(t.Context()); err == nil {
			t.Fatal("accepted malformed approval")
		}
	}
}
