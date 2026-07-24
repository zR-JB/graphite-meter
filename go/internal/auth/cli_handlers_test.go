package auth

import (
	"bytes"
	"context"
	"crypto/sha256"
	"crypto/tls"
	"encoding/base64"
	"net/http"
	"net/http/httptest"
	"net/url"
	"strings"
	"testing"
	"time"
)

// challengeFor returns the base64url challenge a terminal client derives from a
// verifier, matching goclient/auth.go.
func challengeFor(verifier string) string {
	sum := sha256.Sum256([]byte(verifier))
	return base64.RawURLEncoding.EncodeToString(sum[:])
}

func withSessionCookie(r *http.Request, raw string) *http.Request {
	r.AddCookie(&http.Cookie{Name: sessionCookie, Value: raw})
	return r
}

// --- cliPage ---

func TestCliPageRejectsInvalidChallenge(t *testing.T) {
	s := testService(t)
	raw, _, _ := s.createSession("local-operator", "Local operator", "local", time.Time{})
	r := withSessionCookie(secureRequest(http.MethodGet, "/auth/cli?challenge=not-a-challenge", nil), raw)
	rr := httptest.NewRecorder()
	s.cliPage(rr, r)
	if rr.Code != http.StatusForbidden {
		t.Fatalf("invalid challenge code=%d, want 403", rr.Code)
	}
}

func TestCliPageRedirectsToLoginWithoutSession(t *testing.T) {
	s := testService(t)
	challenge := challengeFor("verifier-abc")
	r := secureRequest(http.MethodGet, "/auth/cli?challenge="+challenge, nil)
	rr := httptest.NewRecorder()
	s.cliPage(rr, r)
	if rr.Code != http.StatusSeeOther {
		t.Fatalf("no session code=%d, want 303 redirect", rr.Code)
	}
	if loc := rr.Header().Get("Location"); !strings.HasPrefix(loc, "/login?challenge=") {
		t.Fatalf("redirect Location=%q, want /login carrying the challenge", loc)
	}
}

func TestCliPageRejectsBearerPrincipal(t *testing.T) {
	// Only the browser session may approve: cliPage sees the Bearer principal
	// and redirects to /login, so a grant can never approve itself.
	s := testService(t)
	_, sess, _ := s.createSession("local-operator", "Local operator", "local", time.Time{})
	token := base64.RawURLEncoding.EncodeToString(bytes.Repeat([]byte{7}, 32))
	addGrant(s, sess, token)
	challenge := challengeFor("verifier-bearer")
	r := secureRequest(http.MethodGet, "/auth/cli?challenge="+challenge, nil)
	r.Header.Set("Authorization", "Bearer "+token)
	rr := httptest.NewRecorder()
	s.cliPage(rr, r)
	if rr.Code != http.StatusSeeOther {
		t.Fatalf("bearer on cliPage code=%d, want a /login redirect (never the approval page)", rr.Code)
	}
	if !strings.Contains(rr.Header().Get("Location"), "/login") {
		t.Fatalf("bearer principal not redirected to login: %q", rr.Header().Get("Location"))
	}
}

func TestCliPageRendersApprovalAndReusesIt(t *testing.T) {
	s := testService(t)
	raw, sess, _ := s.createSession("local-operator", "Local operator", "local", time.Time{})
	challenge := challengeFor("verifier-render")
	render := func() *httptest.ResponseRecorder {
		rr := httptest.NewRecorder()
		s.cliPage(rr, withSessionCookie(secureRequest(http.MethodGet, "/auth/cli?challenge="+challenge, nil), raw))
		return rr
	}
	rr := render()
	if rr.Code != http.StatusOK {
		t.Fatalf("valid session code=%d, want 200", rr.Code)
	}
	body := rr.Body.String()
	if !strings.Contains(body, verificationCode(challenge)) {
		t.Error("approval page does not show the verification code")
	}
	if !strings.Contains(body, sess.csrf) {
		t.Error("approval page does not carry the session CSRF token")
	}
	s.mu.Lock()
	a, ok := s.approvals[challenge]
	created := a
	s.mu.Unlock()
	if !ok || a.session != sess || a.approved {
		t.Fatal("first render did not create a pending approval bound to the session")
	}
	render() // second render for the same challenge must reuse, not duplicate
	s.mu.Lock()
	reused := s.approvals[challenge]
	n := len(s.approvals)
	s.mu.Unlock()
	if reused != created || n != 1 {
		t.Fatalf("second render created a new approval (%d total)", n)
	}
}

func TestCliPageCapsApprovalsPerSession(t *testing.T) {
	s := testService(t)
	raw, _, _ := s.createSession("local-operator", "Local operator", "local", time.Time{})
	for i := 0; i < 8; i++ {
		rr := httptest.NewRecorder()
		ch := challengeFor("verifier-cap-" + string(rune('a'+i)))
		s.cliPage(rr, withSessionCookie(secureRequest(http.MethodGet, "/auth/cli?challenge="+ch, nil), raw))
		if rr.Code != http.StatusOK {
			t.Fatalf("approval %d code=%d, want 200", i, rr.Code)
		}
	}
	rr := httptest.NewRecorder()
	s.cliPage(rr, withSessionCookie(secureRequest(http.MethodGet, "/auth/cli?challenge="+challengeFor("verifier-cap-over"), nil), raw))
	if rr.Code != http.StatusForbidden {
		t.Fatalf("ninth approval code=%d, want 403 (per-session cap)", rr.Code)
	}
}

// --- cliApprove ---

func approveRequest(s *Service, sess *session, challenge, csrf, origin string) *http.Request {
	form := url.Values{"csrf": {csrf}, "challenge": {challenge}}.Encode()
	r := httptest.NewRequest(http.MethodPost, s.public.String()+"/auth/cli/approve", strings.NewReader(form))
	r.Host = "meter.example"
	r.TLS = &tls.ConnectionState{}
	r.Header.Set("Content-Type", "application/x-www-form-urlencoded")
	if origin != "" {
		r.Header.Set("Origin", origin)
	}
	return r.WithContext(context.WithValue(r.Context(), principalKey{}, Principal{Subject: sess.subject, session: sess}))
}

func TestCliApproveMarksApprovalApproved(t *testing.T) {
	s := testService(t)
	raw, sess, _ := s.createSession("local-operator", "Local operator", "local", time.Time{})
	challenge := challengeFor("verifier-approve")
	s.cliPage(httptest.NewRecorder(), withSessionCookie(secureRequest(http.MethodGet, "/auth/cli?challenge="+challenge, nil), raw))

	rr := httptest.NewRecorder()
	s.cliApprove(rr, approveRequest(s, sess, challenge, sess.csrf, s.public.String()))
	if rr.Code != http.StatusOK {
		t.Fatalf("approve code=%d, want 200", rr.Code)
	}
	s.mu.Lock()
	approved := s.approvals[challenge].approved
	s.mu.Unlock()
	if !approved {
		t.Fatal("approval was not marked approved")
	}
}

func TestCliApproveRejectsWrongCSRFOriginAndForeignSession(t *testing.T) {
	s := testService(t)
	raw, sess, _ := s.createSession("local-operator", "Local operator", "local", time.Time{})
	_, other, _ := s.createSession("local-operator", "Local operator", "local", time.Time{})
	challenge := challengeFor("verifier-reject")
	s.cliPage(httptest.NewRecorder(), withSessionCookie(secureRequest(http.MethodGet, "/auth/cli?challenge="+challenge, nil), raw))

	cases := map[string]*http.Request{
		"wrong csrf":        approveRequest(s, sess, challenge, "not-the-token", s.public.String()),
		"wrong origin":      approveRequest(s, sess, challenge, sess.csrf, "https://evil.example"),
		"foreign session":   approveRequest(s, other, challenge, other.csrf, s.public.String()),
		"unknown challenge": approveRequest(s, sess, challengeFor("nope"), sess.csrf, s.public.String()),
	}
	for name, r := range cases {
		rr := httptest.NewRecorder()
		s.cliApprove(rr, r)
		if rr.Code != http.StatusForbidden {
			t.Errorf("%s: code=%d, want 403", name, rr.Code)
		}
	}
	s.mu.Lock()
	approved := s.approvals[challenge].approved
	s.mu.Unlock()
	if approved {
		t.Fatal("a rejected request still marked the approval approved")
	}
}

func TestCliApproveRejectsExpiredApproval(t *testing.T) {
	s := testService(t)
	raw, sess, _ := s.createSession("local-operator", "Local operator", "local", time.Time{})
	challenge := challengeFor("verifier-expired")
	s.cliPage(httptest.NewRecorder(), withSessionCookie(secureRequest(http.MethodGet, "/auth/cli?challenge="+challenge, nil), raw))
	s.mu.Lock()
	s.approvals[challenge].expires = s.now().Add(-time.Second)
	s.mu.Unlock()

	rr := httptest.NewRecorder()
	s.cliApprove(rr, approveRequest(s, sess, challenge, sess.csrf, s.public.String()))
	if rr.Code != http.StatusForbidden {
		t.Fatalf("expired approval code=%d, want 403", rr.Code)
	}
}

// --- corsPreflight ---

func TestCorsPreflight(t *testing.T) {
	s := testService(t)
	preflight := func(path, origin, method, headers string, secure bool) *httptest.ResponseRecorder {
		r := secureRequest(http.MethodOptions, path, nil)
		if !secure {
			r.TLS = nil
		}
		if origin != "" {
			r.Header.Set("Origin", origin)
		}
		if method != "" {
			r.Header.Set("Access-Control-Request-Method", method)
		}
		if headers != "" {
			r.Header.Set("Access-Control-Request-Headers", headers)
		}
		rr := httptest.NewRecorder()
		s.corsPreflight(rr, r, secure)
		return rr
	}

	// Happy path: a same-origin GET preflight on a measurement route.
	rr := preflight("/download", s.public.String(), http.MethodGet, "authorization,x-csrf-token", true)
	if rr.Code != http.StatusNoContent {
		t.Fatalf("valid preflight code=%d, want 204", rr.Code)
	}
	if got := rr.Header().Get("Access-Control-Allow-Origin"); got != s.public.String() {
		t.Errorf("Allow-Origin=%q, want the public origin", got)
	}
	if rr.Header().Get("Access-Control-Allow-Credentials") != "true" {
		t.Error("preflight did not allow credentials")
	}

	for name, rr := range map[string]*httptest.ResponseRecorder{
		"insecure":          preflight("/download", s.public.String(), http.MethodGet, "", false),
		"wrong origin":      preflight("/download", "https://evil.example", http.MethodGet, "", true),
		"disallowed method": preflight("/download", s.public.String(), http.MethodDelete, "", true),
		"unlisted path":     preflight("/secret", s.public.String(), http.MethodGet, "", true),
		"disallowed header": preflight("/download", s.public.String(), http.MethodGet, "x-evil", true),
	} {
		if rr.Code != http.StatusForbidden {
			t.Errorf("%s: code=%d, want 403", name, rr.Code)
		}
	}
}
