package goclient

import (
	"context"
	"errors"
	"io"
	"net/http"
	"net/http/httptest"
	"slices"
	"strings"
	"sync/atomic"
	"testing"
	"testing/synctest"
	"time"

	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

func TestBeginAuthorizationStaysOnTheServerHostname(t *testing.T) {
	t.Parallel()
	for _, c := range []struct {
		login    string
		insecure bool
		ok       bool
	}{
		{"https://meter.example:7247/login", false, true},
		{"https://meter.example:7247/login", true, false},
		{"https://login.example:7247/login", false, false},
		{"http://meter.example/login", false, false},
		{"https://meter.example/login?next=x", false, false},
		{"https://meter.example/other", false, false},
	} {
		cfg := DefaultConfig()
		cfg.BaseURL, cfg.InsecureSkipTLSVerify = "https://meter.example:7248", c.insecure
		pending, err := beginAuthorization(cfg, c.login)
		if (err == nil) != c.ok || c.ok && !strings.HasPrefix(pending.BrowserURL, "https://meter.example:7247/auth/") {
			t.Errorf("login %s insecure=%t: %+v, %v", c.login, c.insecure, pending, err)
		}
	}
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
		return &http.Response{StatusCode: http.StatusOK, Body: http.NoBody, Request: r}, nil
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
	if _, err := prepare(t.Context(), cfg, nil, &cred); err == nil || !strings.Contains(err.Error(), "verified HTTPS") {
		t.Fatalf("prepare with a grant and -insecure: %v", err)
	}
	if _, err := wtDial(t.Context(), cred, srv.URL, "/wt/ping", nil); err == nil ||
		!strings.Contains(err.Error(), "refusing") {
		t.Fatalf("WebTransport dial with a grant and -insecure: %v", err)
	}
}

func pendingApproval(t *testing.T, transport roundTripFunc) *PendingAuthorization {
	cfg := DefaultConfig()
	cfg.BaseURL = "https://meter.example"
	pending, err := beginAuthorization(cfg, "https://meter.example/login")
	if err != nil {
		t.Fatal(err)
	}
	pending.client.Transport = transport
	return pending
}

// A redirected poll would re-send the verifier wherever the redirect points.
func TestPollNamesWhyApprovalEnded(t *testing.T) {
	t.Parallel()
	refused := errors.New("connection refused")
	respond := func(status int, header http.Header) roundTripFunc {
		return func(r *http.Request) (*http.Response, error) {
			return &http.Response{StatusCode: status, Header: header, Body: http.NoBody, Request: r}, nil
		}
	}
	for _, c := range []struct {
		name      string
		transport roundTripFunc
		cancelled bool
		want      error
	}{
		{"unreachable", func(*http.Request) (*http.Response, error) { return nil, refused }, false, refused},
		{"pending", respond(http.StatusAccepted, nil), false, ErrApprovalExpired},
		{"blocked", func(r *http.Request) (*http.Response, error) {
			<-r.Context().Done()
			return nil, r.Context().Err()
		}, false, ErrApprovalExpired},
		{"redirected", respond(http.StatusTemporaryRedirect,
			http.Header{"Location": {"https://collector.example/auth/cli/token"}}), false, nil},
		{"cancelled", respond(http.StatusAccepted, nil), true, context.Canceled},
	} {
		synctest.Test(t, func(t *testing.T) {
			var hosts []string
			pending := pendingApproval(t, func(r *http.Request) (*http.Response, error) {
				hosts = append(hosts, r.URL.Host)
				return c.transport(r)
			})
			ctx, cancel := context.WithTimeout(t.Context(), 500*time.Millisecond)
			if c.cancelled {
				cancel()
			}
			defer cancel()
			_, err := pending.Poll(ctx)
			issuerOnly := slices.Equal(hosts, []string{"meter.example"})
			if err == nil || c.want != nil && !errors.Is(err, c.want) || !issuerOnly {
				t.Errorf("%s: %v after requests to %v, want %v from the issuer alone", c.name, err, hosts, c.want)
			}
		})
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
		approval := pendingApproval(t, func(r *http.Request) (*http.Response, error) {
			reply := io.NopCloser(strings.NewReader(body))
			return &http.Response{StatusCode: http.StatusOK, Body: reply, Request: r}, nil
		})
		if _, err := approval.Poll(t.Context()); err == nil {
			t.Fatal("accepted malformed approval")
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
