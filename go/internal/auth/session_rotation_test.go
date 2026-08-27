package auth

import (
	"context"
	"crypto/sha256"
	"crypto/tls"
	"net/http"
	"net/http/httptest"
	"net/url"
	"strings"
	"testing"
)

// addGrant attaches a bearer grant to sess so tests can assert it is revoked together with the session.
func addGrant(s *Service, sess *session, token string) [32]byte {
	h := sha256.Sum256([]byte(token))
	s.mu.Lock()
	defer s.mu.Unlock()
	sess.grants[h] = struct{}{}
	s.grants[h] = sess
	return h
}

func TestRevokeSessionHashDropsSessionAndGrantsButSparesKeep(t *testing.T) {
	s := testService(t)
	_, victim, _ := s.createSession("local-operator", "Local operator", "local")
	gh := addGrant(s, victim, "victim-grant-victim-grant-victim")
	_, keep, _ := s.createSession("local-operator", "Local operator", "local")

	// keep must be spared even when its own hash is passed.
	s.revokeSessionHash(keep.hash, keep)
	s.mu.Lock()
	_, keepAlive := s.sessions[keep.hash]
	s.mu.Unlock()
	if !keepAlive {
		t.Fatal("revokeSessionHash removed the session it was told to keep")
	}

	s.revokeSessionHash(victim.hash, keep)
	s.mu.Lock()
	defer s.mu.Unlock()
	if _, ok := s.sessions[victim.hash]; ok {
		t.Error("victim session survived revocation")
	}
	if _, ok := s.grants[gh]; ok {
		t.Error("victim session's grant survived revocation")
	}
}

func TestDeleteSubjectSessionsRevokesOnlyThatSubject(t *testing.T) {
	s := testService(t)
	_, a, _ := s.createSession("local-operator", "Local operator", "local")
	_, b, _ := s.createSession("local-operator", "Local operator", "local")
	_, other, _ := s.createSession("oidc:someone", "Other", "oidc")
	gh := addGrant(s, b, "b-grant-b-grant-b-grant-b-grant-")

	s.mu.Lock()
	n := s.deleteSubjectSessionsLocked("local-operator")
	s.mu.Unlock()
	if n != 2 {
		t.Fatalf("revoked %d sessions, want 2", n)
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	if _, ok := s.sessions[a.hash]; ok {
		t.Error("session a survived sign-out-everywhere")
	}
	if _, ok := s.sessions[b.hash]; ok {
		t.Error("session b survived sign-out-everywhere")
	}
	if _, ok := s.grants[gh]; ok {
		t.Error("b's grant survived sign-out-everywhere")
	}
	if _, ok := s.sessions[other.hash]; !ok {
		t.Error("a different subject's session must be untouched")
	}
}

func TestPasswordLoginRotatesTheSuppliedSession(t *testing.T) {
	s := testService(t)
	rawPrior, prior, _ := s.createSession("local-operator", "Local operator", "local")
	gh := addGrant(s, prior, "prior-grant-prior-grant-prior-gr")

	token := "abcdefghijklmnopqrstuvwxyz0123456789"
	form := url.Values{"csrf": {token}, "password": {"secret"}}.Encode()
	r := httptest.NewRequest(http.MethodPost, s.public.String()+"/auth/password", strings.NewReader(form))
	r.Host = "meter.example"
	r.TLS = &tls.ConnectionState{}
	r.Header.Set("Content-Type", "application/x-www-form-urlencoded")
	r.Header.Set("Origin", s.public.String())
	r.AddCookie(&http.Cookie{Name: loginCookie, Value: token})
	r.AddCookie(&http.Cookie{Name: sessionCookie, Value: rawPrior})
	rr := httptest.NewRecorder()
	s.passwordLogin(rr, r)

	if rr.Code != http.StatusSeeOther {
		t.Fatalf("login code=%d, want 303", rr.Code)
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	if _, ok := s.sessions[prior.hash]; ok {
		t.Error("prior session survived re-login; it should be rotated out")
	}
	if _, ok := s.grants[gh]; ok {
		t.Error("prior session's grant survived re-login")
	}
	if len(s.sessions) != 1 {
		t.Fatalf("sessions=%d, want only the freshly issued one", len(s.sessions))
	}
}

func TestLogoutEverywhereRevokesEverySubjectSession(t *testing.T) {
	s := testService(t)
	_, a, _ := s.createSession("local-operator", "Local operator", "local")
	_, b, _ := s.createSession("local-operator", "Local operator", "local")
	gh := addGrant(s, b, "b-grant-logout-b-grant-logout-bg")

	form := url.Values{"csrf": {a.csrf}, "scope": {"all"}}.Encode()
	r := httptest.NewRequest(http.MethodPost, s.public.String()+"/auth/logout", strings.NewReader(form))
	r.Header.Set("Content-Type", "application/x-www-form-urlencoded")
	r.Header.Set("Origin", s.public.String())
	r = r.WithContext(context.WithValue(r.Context(), principalKey{}, Principal{Subject: a.subject, session: a}))
	rr := httptest.NewRecorder()
	s.logout(rr, r)

	if rr.Code != http.StatusSeeOther {
		t.Fatalf("logout code=%d, want 303", rr.Code)
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	if _, ok := s.sessions[a.hash]; ok {
		t.Error("current session survived sign-out-everywhere")
	}
	if _, ok := s.sessions[b.hash]; ok {
		t.Error("sibling session survived sign-out-everywhere")
	}
	if _, ok := s.grants[gh]; ok {
		t.Error("sibling session's grant survived sign-out-everywhere")
	}
}

func TestLogoutDefaultScopeSparesSiblingSessions(t *testing.T) {
	s := testService(t)
	_, a, _ := s.createSession("local-operator", "Local operator", "local")
	_, b, _ := s.createSession("local-operator", "Local operator", "local")

	form := url.Values{"csrf": {a.csrf}}.Encode()
	r := httptest.NewRequest(http.MethodPost, s.public.String()+"/auth/logout", strings.NewReader(form))
	r.Header.Set("Content-Type", "application/x-www-form-urlencoded")
	r.Header.Set("Origin", s.public.String())
	r = r.WithContext(context.WithValue(r.Context(), principalKey{}, Principal{Subject: a.subject, session: a}))
	rr := httptest.NewRecorder()
	s.logout(rr, r)

	s.mu.Lock()
	defer s.mu.Unlock()
	if _, ok := s.sessions[a.hash]; ok {
		t.Error("the current session should have been revoked")
	}
	if _, ok := s.sessions[b.hash]; !ok {
		t.Error("a plain sign-out must not touch a sibling session")
	}
}
