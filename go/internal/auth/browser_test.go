package auth

import (
	"context"
	"encoding/base64"
	"encoding/json/v2"
	"io"
	"net/http"
	"net/url"
	"strings"
	"testing"

	"github.com/zR-JB/graphite-meter/go/internal/route"
	"github.com/zR-JB/graphite-meter/go/internal/testkit"
)

const requestingUI = "https://console.example"

func browserExchangeRequest(verifier, origin string) *http.Request {
	r := secureRequest(http.MethodPost, "/auth/browser/token", nil)
	r.Body = io.NopCloser(strings.NewReader(`{"verifier":"` + verifier + `"}`))
	r.Header.Set("Origin", origin)
	r.Header.Set("Content-Type", "application/json")
	return r
}

// Exercise the real approval boundary with a first-party login cookie, then exchange without cookies.
func approveBrowser(t *testing.T, s *Service, cookie string, sess *session) (grant, verifier string) {
	t.Helper()
	handler := mountedAuth(s)
	verifier = randomToken(32)
	challenge := challengeFor(verifier)
	path := "/auth/browser?" + url.Values{"challenge": {challenge}, "client_origin": {requestingUI}}.Encode()
	w := testkit.Record(handler.ServeHTTP, secureRequest(http.MethodGet, path, nil))
	if w.Code != http.StatusSeeOther || w.Header().Get("Location") != "/login?challenge="+challenge {
		t.Fatalf("login continuation: %d %s", w.Code, w.Header().Get("Location"))
	}
	w = testkit.Record(handler.ServeHTTP, withSessionCookie(secureRequest(http.MethodGet, path, nil), cookie))
	if w.Code != http.StatusOK || !strings.Contains(w.Body.String(), requestingUI) {
		t.Fatalf("approval did not identify the exact audience: %d %s", w.Code, w.Body.String())
	}
	w = testkit.Record(handler.ServeHTTP, browserExchangeRequest(verifier, requestingUI))
	if w.Code != http.StatusAccepted {
		t.Fatalf("unapproved exchange: %d", w.Code)
	}
	if w := serveMounted(s, approvalForm("/auth/browser/approve", challenge, cookie, sess)); w.Code != http.StatusOK {
		t.Fatalf("approve: %d", w.Code)
	}
	w = testkit.Record(handler.ServeHTTP, browserExchangeRequest(verifier, requestingUI))
	if w.Code != http.StatusOK || w.Header().Get("Access-Control-Allow-Origin") != requestingUI ||
		w.Header().Get("Access-Control-Allow-Credentials") != "" {
		t.Fatalf("cookie-free exchange failed: %d %v", w.Code, w.Header())
	}
	var result struct {
		Token   string `json:"token"`
		Expires int64  `json:"expires"`
	}
	if err := json.Unmarshal(w.Body.Bytes(), &result); err != nil || result.Token == "" ||
		result.Expires != sess.expires.UnixMilli() {
		t.Fatalf("invalid grant: %v %s", err, w.Body.String())
	}
	w = testkit.Record(handler.ServeHTTP, browserExchangeRequest(verifier, requestingUI))
	if w.Code != http.StatusAccepted {
		t.Fatalf("approval replay: %d", w.Code)
	}
	return result.Token, verifier
}

func browserBearerRequest(path, grant, origin string) *http.Request {
	r := secureRequest(http.MethodGet, path, nil)
	r.Header.Set("Origin", origin)
	r.Header.Set("Authorization", "Bearer "+grant)
	return r
}

func TestCrossSiteBrowserApprovalReentersBeforeReusingStrictSession(t *testing.T) {
	s := testService(t)
	raw, sess, err := s.createSession("subject", "Name", "local")
	if err != nil {
		t.Fatal(err)
	}
	grant, _ := approveBrowser(t, s, raw, sess)
	challenge := randomToken(32)
	path := "/auth/browser?" + url.Values{"challenge": {challenge}, "client_origin": {requestingUI}}.Encode()
	r := secureRequest(http.MethodGet, path, nil)
	r.Header.Set("Sec-Fetch-Site", "cross-site")
	r.Header.Set("Sec-Fetch-Mode", "navigate")
	r.Header.Set("Sec-Fetch-Dest", "document")
	w := testkit.Record(s.browserPage, r)
	if w.Code != http.StatusOK || !strings.Contains(w.Body.String(), `http-equiv="refresh"`) ||
		!strings.Contains(w.Body.String(), "/auth/cli?challenge="+challenge) ||
		strings.Contains(w.Body.String(), "Signed in") {
		t.Fatalf("cross-site entry did not establish a first-party document: %d %s", w.Code, w.Body.String())
	}
	if s.approvals[challenge].session != nil || s.approvals[challenge].approved {
		t.Fatal("the first-party transition authorized the pending request")
	}
	r = withSessionCookie(secureRequest(http.MethodGet, "/auth/cli?challenge="+challenge, nil), raw)
	r.Header.Set("Sec-Fetch-Site", "same-origin")
	w = testkit.Record(s.cliPage, r)
	if w.Code != http.StatusSeeOther || w.Header().Get("Location") != path {
		t.Fatal("first-party entry lost the exact browser approval")
	}
	r = withSessionCookie(secureRequest(http.MethodGet, path, nil), raw)
	r.Header.Set("Sec-Fetch-Site", "same-origin")
	w = testkit.Record(s.browserPage, r)
	if w.Code != http.StatusOK || !strings.Contains(w.Body.String(), requestingUI) ||
		!strings.Contains(w.Body.String(), verificationCode(challenge)) ||
		!strings.Contains(w.Body.String(), "/auth/browser/approve") {
		t.Fatal("existing login did not require explicit approval of the new origin and code")
	}
	if s.approvals[challenge].session != sess || s.approvals[challenge].approved || len(s.sessions) != 1 {
		t.Fatal("approval entry replaced the login or approved the new client")
	}
	if _, ok := s.authenticateGrant(grant); !ok {
		t.Fatal("authorizing another interface revoked the first client's grant")
	}
}

func TestBrowserApprovalKeepsGrantAndCookieScopesSeparate(t *testing.T) {
	s := testService(t)
	raw, sess, err := s.createSession("subject", "Name", "local")
	if err != nil {
		t.Fatal(err)
	}
	grant, _ := approveBrowser(t, s, raw, sess)
	native := grantFor(t, s, sess)
	handler := s.Enforce(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		p, ok := PrincipalFromContext(r.Context())
		if !ok || p.session != sess {
			t.Fatal("grant lost parent admission identity")
		}
		w.WriteHeader(299)
	}), Listener{UI: true})
	for _, tc := range []struct {
		name, path, grant, origin string
		cookie                    bool
		want                      int
	}{
		{"browser discovery", "/preflight", grant, requestingUI, false, 299},
		{"browser upload", "/upload", grant, requestingUI, false, 299},
		{"wrong origin", "/preflight", grant, "https://wrong.example", false, 403},
		{"absent origin", "/preflight", grant, "", false, 403},
		{"catalogue forbidden", "/servers", grant, requestingUI, false, 403},
		{"account forbidden", "/auth/session", grant, requestingUI, false, 403},
		{"cross-site cookies", "/preflight", "", requestingUI, true, 403},
		{"native still rejects remote browser", "/preflight", native, requestingUI, false, 403},
		{"native still works", "/preflight", native, "", false, 299},
	} {
		t.Run(tc.name, func(t *testing.T) {
			r := browserBearerRequest(tc.path, tc.grant, tc.origin)
			if tc.cookie {
				r.Header.Del("Authorization")
				r = withSessionCookie(r, raw)
			}
			w := testkit.Record(handler.ServeHTTP, r)
			if w.Code != tc.want {
				t.Fatalf("status=%d, want %d", w.Code, tc.want)
			}
		})
	}
	second, _ := approveBrowser(t, s, raw, sess)
	p, _ := s.authenticateGrant(grant)
	q, _ := s.authenticateGrant(second)
	if p.grant.id == q.grant.id || p.session != q.session {
		t.Fatal("upload access or parent budget is not correctly scoped")
	}
}

func TestBrowserGrantCapacityKeepsExistingClients(t *testing.T) {
	s := testService(t)
	raw, sess, err := s.createSession("subject", "Name", "local")
	if err != nil {
		t.Fatal(err)
	}
	var grants []string
	for range maxSessionGrants {
		grant, _ := approveBrowser(t, s, raw, sess)
		grants = append(grants, grant)
	}
	verifier := randomToken(32)
	challenge := challengeFor(verifier)
	path := "/auth/browser?" + url.Values{"challenge": {challenge}, "client_origin": {requestingUI}}.Encode()
	page := withSessionCookie(secureRequest(http.MethodGet, path, nil), raw)
	if w := serveMounted(s, page); w.Code != http.StatusTooManyRequests {
		t.Fatalf("approval page at capacity = %d, want 429", w.Code)
	}
	w := serveMounted(s, approvalForm("/auth/browser/approve", challenge, raw, sess))
	if w.Code != http.StatusTooManyRequests || s.approvals[challenge].approved {
		t.Fatalf("approval at capacity = %d, approved=%t", w.Code, s.approvals[challenge].approved)
	}
	for _, exchange := range []struct {
		verifier, origin string
		status           int
	}{
		{verifier, requestingUI, http.StatusTooManyRequests},
		{verifier, "https://wrong.example", http.StatusAccepted},
		{randomToken(32), requestingUI, http.StatusAccepted},
		{"short", requestingUI, http.StatusForbidden},
	} {
		w := serveMounted(s, browserExchangeRequest(exchange.verifier, exchange.origin))
		if w.Code != exchange.status || w.Header().Get("Access-Control-Allow-Origin") != exchange.origin {
			t.Fatalf("exchange from %s = %d %v, want %d", exchange.origin, w.Code, w.Header(), exchange.status)
		}
	}
	if len(sess.grants) != maxSessionGrants || sess.ctx.Err() != nil {
		t.Fatal("capacity recovery changed the existing login or grant budget")
	}
	for _, grant := range grants {
		if _, ok := s.authenticateGrant(grant); !ok {
			t.Fatal("capacity recovery revoked an existing client")
		}
	}
}

// Public approval pages spend neither another client's share of the approval table nor the OIDC callback budget.
func TestPublicBrowserApprovalPagesAreBoundedPerClient(t *testing.T) {
	s := testService(t)
	mux := http.NewServeMux()
	s.Mount(mux)
	handler := s.Enforce(mux, Listener{UI: true})
	const remote = "[2001:db8:1:2::4]:40000"
	request := func(i int, remote string) int {
		challenge := make([]byte, 32)
		challenge[0] = byte(i)
		path := "/auth/browser?" + url.Values{"challenge": {base64.RawURLEncoding.EncodeToString(challenge)},
			"client_origin": {"https://other.example"}}.Encode()
		r := requestFrom(http.MethodGet, path, remote)
		r.Header.Set("Sec-Fetch-Site", "cross-site")
		r.Header.Set("Sec-Fetch-Mode", "no-cors")
		r.Header.Set("Sec-Fetch-Dest", "image")
		w := testkit.Record(handler.ServeHTTP, r)
		return w.Code
	}
	for i := range maxAddressApprovals + 1 {
		want := http.StatusSeeOther
		if i >= maxClientApprovals {
			want = http.StatusForbidden
		}
		if got := request(i, remote); got != want {
			t.Fatalf("public approval request %d status=%d, want %d", i, got, want)
		}
	}
	if len(s.approvals) != maxClientApprovals || request(99, "[2001:db8:1:3::4]:40000") != http.StatusSeeOther {
		t.Fatalf("one /64 holds %d approvals and another was refused", len(s.approvals))
	}
	for range maxAddressExchanges {
		if !s.allowExchange(requestFrom(http.MethodGet, "/auth/oidc/callback", remote)) {
			t.Fatal("unauthenticated approval requests spent the OIDC callback budget")
		}
	}
	if s.allowExchange(requestFrom(http.MethodGet, "/auth/oidc/callback", remote)) {
		t.Fatal("OIDC callbacks lost their own address limit")
	}
}

func TestBrowserSocketTicketsBindAllBoundaries(t *testing.T) {
	s := testService(t)
	raw, sess, err := s.createSession("subject", "Name", "local")
	if err != nil {
		t.Fatal(err)
	}
	grant, _ := approveBrowser(t, s, raw, sess)
	p, _ := s.authenticateGrant(grant)
	for _, tc := range []struct {
		name, path, host, origin string
		want                     bool
	}{
		{"valid", "/ws/ping", "meter.example", requestingUI, true},
		{"route", "/wt/ping", "meter.example", requestingUI, false},
		{"destination", "/ws/ping", "meter.example:8443", requestingUI, false},
		{"origin", "/ws/ping", "meter.example", "https://wrong.example", false},
	} {
		t.Run(tc.name, func(t *testing.T) {
			token := mintTicket(t, s, p, "/ws/ping")
			r := secureRequest(http.MethodGet, tc.path, nil)
			r.Host = tc.host
			r.Header.Set("Origin", tc.origin)
			if _, ok := s.consumeSocketToken(token, r); ok != tc.want {
				t.Fatalf("ticket accepted=%t", ok)
			}
			// Any presentation spends the ticket, so a refused one cannot be retried from the right place.
			valid := secureRequest(http.MethodGet, "/ws/ping", nil)
			valid.Header.Set("Origin", requestingUI)
			if _, ok := s.consumeSocketToken(token, valid); ok {
				t.Fatal("presented ticket accepted again")
			}
		})
	}
}

func TestBrowserApprovalRejectsInsecureAndNonCanonicalAudiences(t *testing.T) {
	s := testService(t)
	challenge := base64.RawURLEncoding.EncodeToString(make([]byte, 32))
	for _, origin := range []string{"http://console.example", "https://console.example/", "https://console.example:443",
		"null", "https://*.example"} {
		query := url.Values{"client_origin": {origin}, "challenge": {challenge}}.Encode()
		r := secureRequest(http.MethodGet, "/auth/browser?"+query, nil)
		w := testkit.Record(s.browserPage, r)
		if w.Code != 403 {
			t.Errorf("audience %q status=%d", origin, w.Code)
		}
	}
	// An expired grant context also prevents ticket minting even before periodic cleanup.
	_, sess, _ := s.createSession("subject", "Name", "local")
	ctx, cancel := context.WithCancel(sess.ctx)
	cancel()
	p := sessionPrincipal(sess, "browser", true)
	p.grant = &grant{sess: sess, origin: requestingUI, ctx: ctx}
	r := secureRequest(http.MethodPost, "/wt/session?target=https://meter.example/wt/ping", nil)
	r = r.WithContext(context.WithValue(r.Context(), principalKey{}, p))
	if _, _, status := s.mintSocketToken(r, route.WebTransport); status != http.StatusForbidden {
		t.Fatal("revoked grant minted a ticket")
	}
}
