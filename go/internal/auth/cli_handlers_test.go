package auth

import (
	"context"
	"crypto/sha256"
	"encoding/base64"
	"encoding/json/v2"
	"fmt"
	"net/http"
	"net/http/httptest"
	"net/url"
	"strings"
	"testing"
	"time"
)

// challengeFor returns the base64url challenge a terminal client derives from a verifier.
func challengeFor(verifier string) string {
	sum := sha256.Sum256([]byte(verifier))
	return base64.RawURLEncoding.EncodeToString(sum[:])
}

func cliPageRequest(challenge, cookie string) *http.Request {
	r := secureRequest(http.MethodGet, "/auth/cli?challenge="+challenge, nil)
	if cookie != "" {
		withSessionCookie(r, cookie)
	}
	return r
}

func cliExchange(s *Service, body string) *httptest.ResponseRecorder {
	rr := httptest.NewRecorder()
	s.token(rr, secureRequest("POST", "/auth/cli/token", strings.NewReader(body)))
	return rr
}

func approveCLI(s *Service, sess *session, verifier string) {
	s.approvals[challengeFor(verifier)] = &approval{session: sess, expires: time.Now().Add(time.Minute),
		approved: true}
}

func TestCliPageRefusals(t *testing.T) {
	s := testService(t)
	raw, sess, _ := s.createSession("local-operator", "Local operator", "local")
	grant := grantFor(t, s, sess)
	bearer := cliPageRequest(challengeFor("verifier-bearer"), "")
	bearer.Header.Set("Authorization", "Bearer "+grant)
	for name, tc := range map[string]struct {
		r    *http.Request
		want int
	}{
		"invalid challenge": {cliPageRequest("not-a-challenge", raw), http.StatusForbidden},
		"no session":        {cliPageRequest(challengeFor("verifier-abc"), ""), http.StatusSeeOther},
		"bearer principal":  {bearer, http.StatusSeeOther},
	} {
		rr := httptest.NewRecorder()
		s.cliPage(rr, tc.r)
		if rr.Code != tc.want || tc.want == http.StatusSeeOther &&
			!strings.HasPrefix(rr.Header().Get("Location"), "/login?challenge=") {
			t.Errorf("%s: code=%d location=%q, want %d", name, rr.Code, rr.Header().Get("Location"), tc.want)
		}
	}
}

func TestCliPageRendersApprovalReusesAndCapsIt(t *testing.T) {
	s := testService(t)
	raw, sess, _ := s.createSession("local-operator", "Local operator", "local")
	challenge := challengeFor("verifier-render")
	for range 2 {
		rr := httptest.NewRecorder()
		s.cliPage(rr, cliPageRequest(challenge, raw))
		body := rr.Body.String()
		if rr.Code != http.StatusOK || !strings.Contains(body, verificationCode(challenge)) ||
			!strings.Contains(body, sess.csrf) {
			t.Fatalf("approval page code=%d lacks the verification code or CSRF token", rr.Code)
		}
	}
	if a := s.approvals[challenge]; len(s.approvals) != 1 || a.session != sess || a.approved {
		t.Fatalf("renders left %d approvals, want one pending approval bound to the session", len(s.approvals))
	}
	for i := 1; i < maxSessionApprovals; i++ {
		rr := httptest.NewRecorder()
		s.cliPage(rr, cliPageRequest(challengeFor(fmt.Sprint("verifier-cap-", i)), raw))
		if rr.Code != http.StatusOK {
			t.Fatalf("approval %d code=%d, want 200", i, rr.Code)
		}
	}
	rr := httptest.NewRecorder()
	s.cliPage(rr, cliPageRequest(challengeFor("verifier-cap-over"), raw))
	if rr.Code != http.StatusForbidden {
		t.Fatalf("approval over the per-session cap code=%d, want 403", rr.Code)
	}
}

func approveRequest(s *Service, sess *session, challenge, csrf, origin string) *http.Request {
	form := url.Values{"csrf": {csrf}, "challenge": {challenge}}.Encode()
	r := secureRequest(http.MethodPost, "/auth/cli/approve", strings.NewReader(form))
	r.Header.Set("Content-Type", "application/x-www-form-urlencoded")
	if origin != "" {
		r.Header.Set("Origin", origin)
	}
	return r.WithContext(context.WithValue(r.Context(), principalKey{},
		Principal{Subject: sess.subject, session: sess}))
}

func TestCliApprove(t *testing.T) {
	s := testService(t)
	raw, sess, _ := s.createSession("local-operator", "Local operator", "local")
	_, other, _ := s.createSession("local-operator", "Local operator", "local")
	challenge, expired := challengeFor("verifier-approve"), challengeFor("verifier-expired")
	s.cliPage(httptest.NewRecorder(), cliPageRequest(challenge, raw))
	s.cliPage(httptest.NewRecorder(), cliPageRequest(expired, raw))
	s.approvals[expired].expires = time.Now().Add(-time.Second)
	for name, r := range map[string]*http.Request{
		"wrong csrf":        approveRequest(s, sess, challenge, "not-the-token", s.origin),
		"wrong origin":      approveRequest(s, sess, challenge, sess.csrf, "https://evil.example"),
		"foreign session":   approveRequest(s, other, challenge, other.csrf, s.origin),
		"unknown challenge": approveRequest(s, sess, challengeFor("nope"), sess.csrf, s.origin),
		"expired approval":  approveRequest(s, sess, expired, sess.csrf, s.origin),
	} {
		rr := httptest.NewRecorder()
		s.approve(rr, r)
		if rr.Code != http.StatusForbidden {
			t.Errorf("%s: code=%d, want 403", name, rr.Code)
		}
	}
	if s.approvals[challenge].approved || s.approvals[expired].approved {
		t.Fatal("a rejected request still marked an approval approved")
	}
	rr := httptest.NewRecorder()
	s.approve(rr, approveRequest(s, sess, challenge, sess.csrf, s.origin))
	if rr.Code != http.StatusOK || !s.approvals[challenge].approved {
		t.Fatalf("approve code=%d, want 200 and an approved approval", rr.Code)
	}
}

func TestCLIExchangeIsSingleUseAndRevokedWithSession(t *testing.T) {
	s := testService(t)
	_, sess, _ := s.createSession("subject", "Name", "local")
	if rr := cliExchange(s, `{"verifier":"not-known"}`); rr.Code != http.StatusAccepted || len(s.approvals) != 0 {
		t.Fatalf("unknown verifier code=%d approvals=%d, want 202 and no state", rr.Code, len(s.approvals))
	}
	approveCLI(s, sess, "strict-json-verifier")
	dup := cliExchange(s, `{"verifier":"unknown","verifier":"strict-json-verifier"}`)
	if dup.Code != http.StatusAccepted || len(sess.grants) != 0 {
		t.Fatalf("duplicate-name request code=%d grants=%d, want 202 and no grant", dup.Code, len(sess.grants))
	}
	approveCLI(s, sess, "terminal-verifier")
	first := cliExchange(s, `{"verifier":"terminal-verifier"}`)
	var out struct {
		Token string `json:"token"`
	}
	if err := json.Unmarshal(first.Body.Bytes(), &out); first.Code != 200 || err != nil || out.Token == "" {
		t.Fatalf("first exchange code=%d body=%s", first.Code, first.Body.String())
	}
	if _, ok := s.authenticateGrant(out.Token); !ok {
		t.Fatal("grant not accepted")
	}
	if replay := cliExchange(s, `{"verifier":"terminal-verifier"}`); replay.Code != http.StatusAccepted {
		t.Fatalf("replay code=%d, want 202", replay.Code)
	}
	s.mu.Lock()
	s.deleteSessionLocked(sess)
	s.mu.Unlock()
	if _, ok := s.authenticateGrant(out.Token); ok {
		t.Fatal("grant survived parent logout")
	}
}

// A CLI login at the grant cap replaces the oldest CLI grant and never a browser grant whose run may be live.
func TestCLIGrantSetIsBoundedWithoutEvictingBrowserGrants(t *testing.T) {
	s := testService(t)
	_, sess, _ := s.createSession("subject", "Name", "local")
	var browser []*grant
	addBrowserGrant := func() {
		_, g := addGrant(s, sess, requestingUI)
		browser = append(browser, g)
	}
	exchange := func(i int) int {
		verifier := fmt.Sprintf("verifier-%d", i)
		approveCLI(s, sess, verifier)
		return cliExchange(s, `{"verifier":"`+verifier+`"}`).Code
	}
	addBrowserGrant()
	for i := range 20 {
		if code := exchange(i); code != http.StatusOK {
			t.Fatalf("exchange %d code=%d, want 200", i, code)
		}
	}
	if len(sess.grants) != maxSessionGrants || browser[0].ctx.Err() != nil {
		t.Fatalf("grants=%d browser grant cancelled=%v, want %d grants with the browser grant live",
			len(sess.grants), browser[0].ctx.Err() != nil, maxSessionGrants)
	}
	for _, g := range sess.grants {
		if g.origin == "" {
			s.deleteGrantLocked(g)
		}
	}
	for len(browser) < maxSessionGrants {
		addBrowserGrant()
	}
	if code := exchange(99); code != http.StatusTooManyRequests {
		t.Fatalf("CLI exchange against %d browser grants = %d, want 429", maxSessionGrants, code)
	}
	for i, g := range browser {
		if g.ctx.Err() != nil {
			t.Fatalf("browser grant %d was cancelled by a CLI login", i)
		}
	}
}
