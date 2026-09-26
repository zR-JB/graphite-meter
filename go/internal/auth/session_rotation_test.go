package auth

import (
	"context"
	"crypto/sha256"
	"net/http"
	"net/http/httptest"
	"net/url"
	"strings"
	"testing"
)

func addGrant(s *Service, sess *session, origin string) (string, *grant) {
	raw := randomToken(32)
	ctx, cancel := context.WithCancel(sess.ctx)
	s.mu.Lock()
	defer s.mu.Unlock()
	s.grantSeq++
	g := &grant{sess: sess, key: sha256.Sum256([]byte(raw)), origin: origin, seq: s.grantSeq, ctx: ctx, cancel: cancel}
	s.grants[g.key], sess.grants[g.key] = g, g
	return raw, g
}

func grantFor(t *testing.T, s *Service, sess *session) string {
	raw, _ := addGrant(s, sess, "")
	return raw
}

func TestPasswordLoginRotatesTheSuppliedSession(t *testing.T) {
	s := testService(t)
	rawPrior, prior, _ := s.createSession("local-operator", "Local operator", "local")
	grant := grantFor(t, s, prior)
	token := "abcdefghijklmnopqrstuvwxyz0123456789"
	form := url.Values{"csrf": {token}, "password": {"secret"}}.Encode()
	r := withSessionCookie(secureRequest(http.MethodPost, "/auth/password", strings.NewReader(form)), rawPrior)
	r.Header.Set("Content-Type", "application/x-www-form-urlencoded")
	r.Header.Set("Origin", s.origin)
	r.AddCookie(&http.Cookie{Name: loginCookie, Value: token})
	rr := httptest.NewRecorder()
	s.passwordLogin(rr, r)
	if rr.Code != http.StatusSeeOther {
		t.Fatalf("login code=%d, want 303", rr.Code)
	}
	if _, ok := s.authenticateGrant(grant); ok || s.sessions[prior.hash] != nil || len(s.sessions) != 1 {
		t.Fatal("re-login kept the prior session or its grant")
	}
}

func TestLogoutScope(t *testing.T) {
	for _, scope := range []string{"", "all"} {
		t.Run("scope="+scope, func(t *testing.T) {
			s := testService(t)
			_, current, _ := s.createSession("local-operator", "Local operator", "local")
			_, sibling, _ := s.createSession("local-operator", "Local operator", "local")
			_, other, _ := s.createSession("oidc:someone", "Other", "oidc")
			grant := grantFor(t, s, sibling)
			form := url.Values{"csrf": {current.csrf}, "scope": {scope}}.Encode()
			r := secureRequest(http.MethodPost, "/auth/logout", strings.NewReader(form))
			r.Header.Set("Content-Type", "application/x-www-form-urlencoded")
			r.Header.Set("Origin", s.origin)
			p := Principal{Subject: current.subject, session: current}
			rr := httptest.NewRecorder()
			s.logout(rr, r.WithContext(context.WithValue(r.Context(), principalKey{}, p)))
			if rr.Code != http.StatusSeeOther || s.sessions[current.hash] != nil || s.sessions[other.hash] == nil {
				t.Fatalf("logout code=%d revoked the wrong sessions", rr.Code)
			}
			cleared := map[string]bool{}
			for _, c := range rr.Result().Cookies() {
				cleared[c.Name] = c.Value == "" && c.MaxAge < 0
			}
			if !cleared[sessionCookie] || !cleared[csrfCookie] || !cleared[loginCookie] {
				t.Fatalf("logout cleared cookies %v, want the session, CSRF and login cookies", cleared)
			}
			_, grantLive := s.authenticateGrant(grant)
			if everywhere := scope == "all"; (s.sessions[sibling.hash] == nil) != everywhere ||
				grantLive == everywhere {
				t.Fatalf("sibling session and grant live=%t/%t after scope %q", s.sessions[sibling.hash] != nil,
					grantLive, scope)
			}
		})
	}
}
