package auth

import (
	"crypto/sha256"
	"encoding/base32"
	"encoding/base64"
	jsonv2 "encoding/json/v2"
	"maps"
	"net/http"
	"net/url"
	"time"
)

const (
	maxApprovals        = 256
	maxSessionApprovals = 8
	maxSessionGrants    = 8
	approvalLifetime    = 2 * time.Minute
)

type cliApproval struct {
	code     string
	session  *session
	expires  time.Time
	approved bool
}

func validChallenge(v string) bool {
	b, err := base64.RawURLEncoding.DecodeString(v)
	return err == nil && len(b) == 32 && len(v) <= 64
}

func challengeOrEmpty(v string) string {
	if validChallenge(v) {
		return v
	}
	return ""
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
	maps.DeleteFunc(s.approvals, func(_ string, pending *cliApproval) bool { return !now.Before(pending.expires) })
	approval := s.approvals[challenge]
	if approval == nil {
		count := 0
		for pending := range maps.Values(s.approvals) {
			if pending.session == p.session {
				count++
			}
		}
		if len(s.approvals) >= maxApprovals || count >= maxSessionApprovals {
			s.mu.Unlock()
			s.counters.capacity.Add(1)
			forbidden(w)
			return
		}
		approval = &cliApproval{code: verificationCode(challenge), session: p.session, expires: now.Add(approvalLifetime)}
		s.approvals[challenge] = approval
	}
	s.mu.Unlock()
	csrf := p.session.csrf
	w.Header().Set("Content-Type", "text/html; charset=utf-8")
	_ = cliTemplate.Execute(w, map[string]any{"Styles": authStyles, "Code": approval.code, "Challenge": challenge, "CSRF": csrf})
}

func (s *Service) cliApprove(w http.ResponseWriter, r *http.Request) {
	securityHeaders(w.Header())
	r.Body = http.MaxBytesReader(w, r.Body, 4096)
	if err := r.ParseForm(); err != nil {
		forbidden(w)
		return
	}
	p, ok := s.sessionFormPrincipal(r)
	if !ok || p.Bearer {
		forbidden(w)
		return
	}
	challenge := r.FormValue("challenge")
	s.mu.Lock()
	approval := s.approvals[challenge]
	if approval == nil || approval.session != p.session || !s.now().Before(approval.expires) {
		s.mu.Unlock()
		forbidden(w)
		return
	}
	approval.approved = true
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
	if err := jsonv2.UnmarshalRead(r.Body, &req); err != nil || len(req.Verifier) > 128 {
		s.writeGrantPending(w)
		return
	}
	sum := sha256.Sum256([]byte(req.Verifier))
	challenge := base64.RawURLEncoding.EncodeToString(sum[:])
	now := s.now()
	s.mu.Lock()
	approval := s.approvals[challenge]
	if approval == nil || !approval.approved || !now.Before(approval.expires) || !now.Before(approval.session.expires) || approval.session.ctx.Err() != nil {
		s.mu.Unlock()
		s.writeGrantPending(w)
		return
	}
	grant := randomToken(32)
	delete(s.approvals, challenge)
	h := sha256.Sum256([]byte(grant))
	sess := approval.session
	if len(sess.grants) >= maxSessionGrants {
		for old := range maps.Keys(sess.grants) {
			delete(sess.grants, old)
			delete(s.grants, old)
			break
		}
	}
	sess.grants[h] = struct{}{}
	s.grants[h] = sess
	expires := sess.expires
	s.mu.Unlock()
	w.Header().Set("Content-Type", "application/json")
	_ = jsonv2.MarshalWrite(w, map[string]any{"token": grant, "expires": expires})
}

func (s *Service) writeGrantPending(w http.ResponseWriter) {
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(http.StatusAccepted)
	_, _ = w.Write([]byte(`{"status":"pending"}`))
}
