package auth

import (
	"crypto/sha256"
	"encoding/base32"
	"encoding/base64"
	"encoding/json"
	"net/http"
	"net/url"
	"time"
)

// cliApproval is one pending browser approval of a native-client grant: a
// verification code shown on both sides, bound to the approving session, valid
// for two minutes and never persisted.
type cliApproval struct {
	challenge string
	code      string
	session   *session
	expires   time.Time
	approved  bool
}

func validChallenge(v string) bool {
	b, err := base64.RawURLEncoding.DecodeString(v)
	return err == nil && len(b) == 32 && len(v) <= 64
}
func verificationCode(challenge string) string {
	value, err := base64.RawURLEncoding.DecodeString(challenge)
	if err != nil || len(value) < 5 {
		return ""
	}
	return base32.StdEncoding.WithPadding(base32.NoPadding).EncodeToString(value[:5])
}

func (s *Service) cliPage(w http.ResponseWriter, r *http.Request) {
	securityHeaders(w.Header())
	challenge := r.URL.Query().Get("challenge")
	if !validChallenge(challenge) {
		forbidden(w)
		return
	}
	p, ok := s.authenticate(r)
	if !ok || p.session == nil || p.Bearer {
		http.Redirect(w, r, "/login?challenge="+url.QueryEscape(challenge), http.StatusSeeOther)
		return
	}
	now := s.now()
	s.mu.Lock()
	for k, a := range s.approvals {
		if !now.Before(a.expires) {
			delete(s.approvals, k)
		}
	}
	a := s.approvals[challenge]
	if a == nil {
		count := 0
		for _, x := range s.approvals {
			if x.session == p.session {
				count++
			}
		}
		if len(s.approvals) >= 256 || count >= 8 {
			s.mu.Unlock()
			s.counters.capacity.Add(1)
			forbidden(w)
			return
		}
		a = &cliApproval{challenge: challenge, code: verificationCode(challenge), session: p.session, expires: now.Add(2 * time.Minute)}
		s.approvals[challenge] = a
	}
	s.mu.Unlock()
	csrf := p.session.csrf
	w.Header().Set("Content-Type", "text/html; charset=utf-8")
	_ = cliTemplate.Execute(w, map[string]any{"Styles": authStyles, "Code": a.code, "Challenge": challenge, "CSRF": csrf})
}

func (s *Service) cliApprove(w http.ResponseWriter, r *http.Request) {
	securityHeaders(w.Header())
	r.Body = http.MaxBytesReader(w, r.Body, 4096)
	if err := r.ParseForm(); err != nil {
		forbidden(w)
		return
	}
	p, ok := PrincipalFromContext(r.Context())
	if !ok || p.session == nil || p.Bearer || r.Header.Get("Origin") != s.public.String() || !constantEqual(p.session.csrf, r.FormValue("csrf")) {
		forbidden(w)
		return
	}
	challenge := r.FormValue("challenge")
	s.mu.Lock()
	a := s.approvals[challenge]
	if a == nil || a.session != p.session || !s.now().Before(a.expires) {
		s.mu.Unlock()
		forbidden(w)
		return
	}
	a.approved = true
	s.counters.cliApproval.Add(1)
	s.mu.Unlock()
	w.Header().Set("Content-Type", "text/html; charset=utf-8")
	_ = cliDoneTemplate.Execute(w, map[string]any{"Styles": authStyles})
}

func (s *Service) cliToken(w http.ResponseWriter, r *http.Request) {
	securityHeaders(w.Header())
	r.Body = http.MaxBytesReader(w, r.Body, 4096)
	var req struct {
		Verifier string `json:"verifier"`
	}
	if err := json.NewDecoder(r.Body).Decode(&req); err != nil || len(req.Verifier) > 128 {
		s.pending(w)
		return
	}
	sum := sha256.Sum256([]byte(req.Verifier))
	challenge := base64.RawURLEncoding.EncodeToString(sum[:])
	now := s.now()
	s.mu.Lock()
	a := s.approvals[challenge]
	if a == nil || !a.approved || !now.Before(a.expires) || !now.Before(a.session.expires) || a.session.ctx.Err() != nil {
		s.mu.Unlock()
		s.pending(w)
		return
	}
	delete(s.approvals, challenge)
	grant, err := randomToken(32)
	if err != nil {
		s.mu.Unlock()
		s.pending(w)
		return
	}
	h := sha256.Sum256([]byte(grant))
	if len(a.session.grants) >= 8 {
		for old := range a.session.grants {
			delete(a.session.grants, old)
			delete(s.grants, old)
			break
		}
	}
	a.session.grants[h] = struct{}{}
	s.grants[h] = a.session
	expires := a.session.expires
	s.mu.Unlock()
	w.Header().Set("Content-Type", "application/json")
	_ = json.NewEncoder(w).Encode(map[string]any{"token": grant, "expires": expires})
}
func (s *Service) pending(w http.ResponseWriter) {
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(http.StatusAccepted)
	_, _ = w.Write([]byte(`{"status":"pending"}`))
}
