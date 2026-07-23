package auth

// handlers.go is the login surface: the server-rendered login page and the
// password, session-info, and logout handlers mounted behind Enforce.

import (
	"encoding/json"
	"net/http"
	"net/url"
	"time"
)

func (s *Service) loginPage(w http.ResponseWriter, r *http.Request) {
	s.loginSecurityHeaders(w.Header())
	csrf, err := randomToken(32)
	if err != nil {
		http.Error(w, "temporarily unavailable", http.StatusServiceUnavailable)
		return
	}
	setSessionCookie(w, loginCookie, csrf, s.now().Add(10*time.Minute))
	data := loginView{
		Styles: authStyles, CSRF: csrf,
		Password:  s.cfg.Mode == "password" || s.cfg.Mode == "hybrid",
		OIDC:      s.cfg.Mode == "oidc" || s.cfg.Mode == "hybrid",
		OIDCReady: s.oidc != nil && s.oidc.ready(), Provider: s.cfg.OIDCProviderName,
		Error: r.URL.Query().Get("error") != "", Expired: r.URL.Query().Get("reason") == "expired",
		Challenge: r.URL.Query().Get("challenge"),
	}
	renderLogin(w, s.loginTemplate, data)
}

func (s *Service) passwordLogin(w http.ResponseWriter, r *http.Request) {
	securityHeaders(w.Header())
	if s.cfg.Mode != "password" && s.cfg.Mode != "hybrid" {
		http.NotFound(w, r)
		return
	}
	r.Body = http.MaxBytesReader(w, r.Body, 4096)
	if err := r.ParseForm(); err != nil {
		s.debugln("local password rejected reason=malformed_form")
		s.loginFailure(w, r)
		return
	}
	if reason := s.csrfFailure(r, "csrf"); reason != "" {
		s.debugln("local password rejected reason=csrf_" + reason)
		s.loginFailure(w, r)
		return
	}
	if !s.allowAttempt(r) {
		s.debugln("local password rejected reason=rate_limit_or_client_address")
		s.counters.throttled.Add(1)
		s.loginFailure(w, r)
		return
	}
	select {
	case s.argon <- struct{}{}:
		defer func() { <-s.argon }()
	default:
		s.debugln("local password rejected reason=verifier_busy")
		s.loginFailure(w, r)
		return
	}
	if !verifyPassword(s.passwordHash, r.FormValue("password")) {
		s.debugln("local password rejected reason=mismatch")
		s.counters.invalidPassword.Add(1)
		s.loginFailure(w, r)
		return
	}
	raw, sess, err := s.createSession("local-operator", "Local operator", "local", time.Time{})
	if err != nil {
		s.debugln("local password rejected reason=session_capacity")
		s.loginFailure(w, r)
		return
	}
	setSessionCookie(w, sessionCookie, raw, sess.expires)
	setCSRFCookie(w, sess.csrf, sess.expires)
	s.counters.local.Add(1)
	clearCookie(w, loginCookie)
	dest := "/"
	if challenge := r.FormValue("challenge"); challenge != "" {
		dest = "/auth/cli?challenge=" + url.QueryEscape(challenge)
	}
	http.Redirect(w, r, dest, http.StatusSeeOther)
}

// loginFailure collapses every credential outcome into one generic response so
// the page cannot be used to enumerate valid inputs.
func (s *Service) loginFailure(w http.ResponseWriter, r *http.Request) {
	query := url.Values{"error": {"1"}}
	if challenge := r.FormValue("challenge"); validChallenge(challenge) {
		query.Set("challenge", challenge)
	}
	http.Redirect(w, r, "/login?"+query.Encode(), http.StatusSeeOther)
}

func (s *Service) sessionInfo(w http.ResponseWriter, r *http.Request) {
	securityHeaders(w.Header())
	p, ok := PrincipalFromContext(r.Context())
	if !ok {
		forbidden(w)
		return
	}
	w.Header().Set("Content-Type", "application/json")
	_ = json.NewEncoder(w).Encode(map[string]any{"name": p.Name, "provider": p.Provider, "expires": p.Expires, "csrf": p.session.csrf})
}

func (s *Service) logout(w http.ResponseWriter, r *http.Request) {
	securityHeaders(w.Header())
	r.Body = http.MaxBytesReader(w, r.Body, 4096)
	if err := r.ParseForm(); err != nil {
		forbidden(w)
		return
	}
	p, ok := PrincipalFromContext(r.Context())
	if !ok || p.session == nil || r.Header.Get("Origin") != s.public.String() || !constantEqual(p.session.csrf, r.FormValue("csrf")) {
		forbidden(w)
		return
	}
	s.mu.Lock()
	s.deleteSessionLocked(p.session)
	s.mu.Unlock()
	s.counters.logout.Add(1)
	clearCookie(w, sessionCookie)
	clearCookie(w, loginCookie)
	clearCookie(w, csrfCookie)
	http.Redirect(w, r, "/login", http.StatusSeeOther)
}
