package auth

import (
	"crypto/sha256"
	"encoding/base32"
	"encoding/base64"
	"encoding/json/v2"
	"html/template"
	"maps"
	"net/http"
	"net/url"
	"time"

	"github.com/zR-JB/graphite-meter/go/internal/transport"
)

const (
	maxApprovals        = 256
	maxSessionApprovals = 8
	maxClientApprovals  = 8
	maxSessionGrants    = 8
	approvalLifetime    = 2 * time.Minute
)

type cliApproval struct {
	browserOrigin string
	client        string
	code          string
	session       *session
	expires       time.Time
	approved      bool
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

func render(w http.ResponseWriter, tmpl *template.Template, data any) {
	w.Header().Set("Content-Type", "text/html; charset=utf-8")
	_ = tmpl.Execute(w, data)
}

func writeJSON(w http.ResponseWriter, value any) {
	w.Header().Set("Content-Type", "application/json")
	_ = json.MarshalWrite(w, value)
}

func loginRedirect(w http.ResponseWriter, r *http.Request, challenge string) {
	http.Redirect(w, r, "/login?challenge="+url.QueryEscape(challenge), http.StatusSeeOther)
}

// approvalRoomLocked prunes expired approvals and reports whether sess and client may start another.
func (s *Service) approvalRoomLocked(sess *session, client string, now time.Time) (bool, bool) {
	maps.DeleteFunc(s.approvals, func(_ string, a *cliApproval) bool { return !now.Before(a.expires) })
	bySession, byClient := 0, 0
	for a := range maps.Values(s.approvals) {
		if a.session == sess {
			bySession++
		}
		if a.client == client {
			byClient++
		}
	}
	return bySession < maxSessionApprovals, byClient < maxClientApprovals && len(s.approvals) < maxApprovals
}

func (s *Service) approvalClient(r *http.Request) (string, bool) {
	addr, ok := s.authClientAddress(r)
	return transport.AddressBucket(addr), ok
}

func (s *Service) cliPage(w http.ResponseWriter, r *http.Request) {
	securityHeaders(w.Header())
	challenge := r.URL.Query().Get("challenge")
	if !validChallenge(challenge) {
		forbidden(w)
		return
	}
	if dest := s.browserApprovalRedirect(challenge); dest != "" {
		http.Redirect(w, r, dest, http.StatusSeeOther)
		return
	}
	p, ok := s.authenticate(r)
	if !ok || p.session == nil || p.Bearer {
		loginRedirect(w, r, challenge)
		return
	}
	client, ok := s.approvalClient(r)
	if !ok {
		forbidden(w)
		return
	}
	now := time.Now()
	s.mu.Lock()
	sessionRoom, clientRoom := s.approvalRoomLocked(p.session, client, now)
	approval := s.approvals[challenge]
	if approval == nil {
		if !sessionRoom || !clientRoom {
			s.mu.Unlock()
			s.count(countCapacity)
			forbidden(w)
			return
		}
		approval = &cliApproval{code: verificationCode(challenge), session: p.session, client: client,
			expires: now.Add(approvalLifetime)}
		s.approvals[challenge] = approval
	}
	s.mu.Unlock()
	render(w, cliTemplate, map[string]any{
		"Styles": authStyles, "Code": approval.code, "Challenge": challenge, "CSRF": p.session.csrf,
	})
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
	browser := r.URL.Path == "/auth/browser/approve"
	s.mu.Lock()
	approval := s.approvals[r.FormValue("challenge")]
	if approval == nil || approval.session != p.session || (approval.browserOrigin != "") != browser ||
		!time.Now().Before(approval.expires) {
		s.mu.Unlock()
		forbidden(w)
		return
	}
	if browser && len(p.session.grants) >= maxSessionGrants {
		s.mu.Unlock()
		writeBrowserGrantCapacity(w, approval.browserOrigin)
		return
	}
	approval.approved = true
	s.mu.Unlock()
	s.count(countCLIApproval)
	render(w, cliDoneTemplate, map[string]any{"Styles": authStyles, "Browser": browser})
}

func (s *Service) approvalLocked(verifier string, now time.Time) (string, *cliApproval) {
	sum := sha256.Sum256([]byte(verifier))
	challenge := base64.RawURLEncoding.EncodeToString(sum[:])
	a := s.approvals[challenge]
	if a == nil || a.session == nil || !now.Before(a.expires) || !now.Before(a.session.expires) ||
		a.session.ctx.Err() != nil {
		return challenge, nil
	}
	return challenge, a
}

func (s *Service) issueGrantLocked(challenge string, sess *session) (string, [32]byte) {
	delete(s.approvals, challenge)
	raw := randomToken(32)
	key := sha256.Sum256([]byte(raw))
	s.grantSeq++
	sess.grants[key] = s.grantSeq
	return raw, key
}

func readVerifier(w http.ResponseWriter, r *http.Request) (string, bool) {
	r.Body = http.MaxBytesReader(w, r.Body, 4096)
	var req struct {
		Verifier string `json:"verifier"`
	}
	err := json.UnmarshalRead(r.Body, &req)
	return req.Verifier, err == nil && len(req.Verifier) <= 128
}

func (s *Service) cliToken(w http.ResponseWriter, r *http.Request) {
	securityHeaders(w.Header())
	verifier, ok := readVerifier(w, r)
	if !ok {
		writeGrantPending(w)
		return
	}
	s.mu.Lock()
	challenge, approval := s.approvalLocked(verifier, time.Now())
	if approval == nil || approval.browserOrigin != "" || !approval.approved {
		s.mu.Unlock()
		writeGrantPending(w)
		return
	}
	sess := approval.session
	if len(sess.grants) >= maxSessionGrants {
		oldest, found := s.oldestCLIGrantLocked(sess)
		if !found {
			s.mu.Unlock()
			w.WriteHeader(http.StatusTooManyRequests)
			return
		}
		delete(sess.grants, oldest)
		s.deleteGrantLocked(oldest)
	}
	grant, key := s.issueGrantLocked(challenge, sess)
	s.grants[key] = sess
	s.mu.Unlock()
	writeJSON(w, map[string]any{"token": grant, "expires": sess.expires})
}

func (s *Service) oldestCLIGrantLocked(sess *session) (oldest [32]byte, found bool) {
	var issued uint64
	for grant, seq := range sess.grants {
		if _, browser := s.browserGrants[grant]; !browser && (!found || seq < issued) {
			oldest, issued, found = grant, seq, true
		}
	}
	return oldest, found
}

func writeGrantPending(w http.ResponseWriter) {
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(http.StatusAccepted)
	_, _ = w.Write([]byte(`{"status":"pending"}`))
}
