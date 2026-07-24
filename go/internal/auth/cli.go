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

const (
	maxApprovals        = 256
	maxSessionApprovals = 8
	maxSessionGrants    = 8
	approvalLifetime    = 2 * time.Minute
)

// cliApproval is one pending browser approval of a native-client grant: a
// verification code shown on both sides, bound to the approving session, valid
// for approvalLifetime and never persisted.
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
	for key, pending := range s.approvals {
		if !now.Before(pending.expires) {
			delete(s.approvals, key)
		}
	}
	approval := s.approvals[challenge]
	if approval == nil {
		count := 0
		for _, pending := range s.approvals {
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
	p, ok := PrincipalFromContext(r.Context())
	if !ok || p.session == nil || p.Bearer || r.Header.Get("Origin") != s.public.String() || !constantEqual(p.session.csrf, r.FormValue("csrf")) {
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
	if err := json.NewDecoder(r.Body).Decode(&req); err != nil || len(req.Verifier) > 128 {
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
	grant, err := randomToken(32)
	if err != nil {
		s.mu.Unlock()
		s.writeGrantPending(w)
		return
	}
	// The approval is consumed only once a grant exists: an RNG failure must not
	// cost the operator a second browser confirmation.
	delete(s.approvals, challenge)
	h := sha256.Sum256([]byte(grant))
	sess := approval.session
	if len(sess.grants) >= maxSessionGrants {
		for old := range sess.grants {
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
	_ = json.NewEncoder(w).Encode(map[string]any{"token": grant, "expires": expires})
}

func (s *Service) writeGrantPending(w http.ResponseWriter) {
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(http.StatusAccepted)
	_, _ = w.Write([]byte(`{"status":"pending"}`))
}
