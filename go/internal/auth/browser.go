package auth

import (
	"context"
	"crypto/sha256"
	"encoding/base64"
	"encoding/json/v2"
	"maps"
	"net/http"
	"net/url"
	"strings"

	"github.com/zR-JB/graphite-meter/go/internal/cors"
	"github.com/zR-JB/graphite-meter/go/internal/route"
	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

type browserGrant struct {
	sess       *session
	origin, id string
	ctx        context.Context
	cancel     context.CancelFunc
}

func secureBrowserOrigin(raw string) (string, bool) {
	canonical, err := wire.CanonicalOrigin(raw)
	return canonical, err == nil && strings.HasPrefix(canonical, "https://") && raw == canonical
}

func (s *Service) browserPage(w http.ResponseWriter, r *http.Request) {
	securityHeaders(w.Header())
	challenge := r.URL.Query().Get("challenge")
	clientOrigin, valid := secureBrowserOrigin(r.URL.Query().Get("client_origin"))
	if !validChallenge(challenge) || !valid {
		forbidden(w)
		return
	}
	now := s.now()
	s.mu.Lock()
	maps.DeleteFunc(s.approvals, func(_ string, a *cliApproval) bool { return !now.Before(a.expires) })
	a := s.approvals[challenge]
	s.mu.Unlock()
	if a == nil && !s.allowBrowserApproval(r) {
		forbidden(w)
		return
	}
	s.mu.Lock()
	a = s.approvals[challenge]
	if a == nil && len(s.approvals) < maxApprovals {
		a = &cliApproval{code: verificationCode(challenge), expires: now.Add(approvalLifetime), browserOrigin: clientOrigin}
		s.approvals[challenge] = a
	}
	valid = a != nil && a.browserOrigin == clientOrigin
	s.mu.Unlock()
	if !valid {
		forbidden(w)
		return
	}
	p, authenticated := s.authenticate(r)
	if !authenticated || p.Bearer || p.session == nil {
		http.Redirect(w, r, "/login?challenge="+url.QueryEscape(challenge), http.StatusSeeOther)
		return
	}
	s.mu.Lock()
	count := 0
	for pending := range maps.Values(s.approvals) {
		if pending.session == p.session {
			count++
		}
	}
	valid = s.approvals[challenge] == a && now.Before(a.expires) && (a.session == p.session || a.session == nil && count < maxSessionApprovals)
	if valid {
		a.session = p.session
	}
	atCapacity := valid && len(p.session.grants) >= maxSessionGrants
	s.mu.Unlock()
	if !valid {
		forbidden(w)
		return
	}
	if atCapacity {
		writeBrowserGrantCapacity(w, clientOrigin)
		return
	}
	w.Header().Set("Content-Type", "text/html; charset=utf-8")
	_ = cliTemplate.Execute(w, map[string]any{"Styles": authStyles, "Code": a.code, "Challenge": challenge, "CSRF": p.session.csrf, "BrowserOrigin": clientOrigin})
}

func writeBrowserGrantCapacity(w http.ResponseWriter, clientOrigin string) {
	w.Header().Set("Content-Type", "text/html; charset=utf-8")
	w.WriteHeader(http.StatusTooManyRequests)
	_ = cliTemplate.Execute(w, map[string]any{"Styles": authStyles, "BrowserOrigin": clientOrigin, "BrowserCapacity": true, "ClientLimit": maxSessionGrants})
}

func (s *Service) browserToken(w http.ResponseWriter, r *http.Request) {
	securityHeaders(w.Header())
	clientOrigin, valid := secureBrowserOrigin(r.Header.Get("Origin"))
	if !valid {
		forbidden(w)
		return
	}
	cors.Bearer(w.Header(), clientOrigin)
	r.Body = http.MaxBytesReader(w, r.Body, 4096)
	var req struct {
		Verifier string `json:"verifier"`
	}
	if err := json.UnmarshalRead(r.Body, &req); err != nil || len(req.Verifier) < 32 || len(req.Verifier) > 128 {
		forbidden(w)
		return
	}
	hash := sha256.Sum256([]byte(req.Verifier))
	challenge := base64.RawURLEncoding.EncodeToString(hash[:])
	now := s.now()
	s.mu.Lock()
	a := s.approvals[challenge]
	if a == nil || a.browserOrigin != clientOrigin || a.session == nil || !now.Before(a.expires) || !now.Before(a.session.expires) || a.session.ctx.Err() != nil {
		s.mu.Unlock()
		s.writeGrantPending(w)
		return
	}
	sess := a.session
	if len(sess.grants) >= maxSessionGrants {
		s.mu.Unlock()
		w.WriteHeader(http.StatusTooManyRequests)
		return
	}
	if !a.approved {
		s.mu.Unlock()
		s.writeGrantPending(w)
		return
	}
	raw := randomToken(32)
	key := sha256.Sum256([]byte(raw))
	ctx, cancel := context.WithCancel(sess.ctx)
	s.browserGrants[key] = &browserGrant{sess: sess, origin: clientOrigin, id: randomToken(16), ctx: ctx, cancel: cancel}
	sess.grants[key] = struct{}{}
	delete(s.approvals, challenge)
	s.mu.Unlock()
	w.Header().Set("Content-Type", "application/json")
	_ = json.MarshalWrite(w, map[string]any{"token": raw, "expires": sess.expires.UnixMilli(), "remainingMs": sess.expires.Sub(now).Milliseconds(), "maximumLifetimeMs": sessionLifetime.Milliseconds()})
}

func (s *Service) browserApprovalRedirect(challenge string) string {
	s.mu.Lock()
	defer s.mu.Unlock()
	if a := s.approvals[challenge]; a != nil && a.browserOrigin != "" && s.now().Before(a.expires) {
		return "/auth/browser?" + url.Values{"challenge": {challenge}, "client_origin": {a.browserOrigin}}.Encode()
	}
	return ""
}

func (s *Service) deleteGrantLocked(hash [32]byte) {
	delete(s.grants, hash)
	if g := s.browserGrants[hash]; g != nil {
		g.cancel()
		delete(s.browserGrants, hash)
	}
}

// BrowserOrigin is supplied only after a browser grant or its socket ticket authenticated.
func BrowserOrigin(r *http.Request) string {
	p, ok := PrincipalFromContext(r.Context())
	if !ok {
		return ""
	}
	return p.BrowserOrigin
}

func (p Principal) measurementContext() context.Context {
	if p.browserGrant != nil {
		return p.browserGrant.ctx
	}
	return p.session.ctx
}

func (p Principal) MeasurementOwner() string {
	if p.browserGrant != nil {
		return "principal:" + p.Subject + "\x00browser-grant:" + p.browserGrant.id
	}
	return ""
}

func (s *Service) browserPreflight(w http.ResponseWriter, r *http.Request) bool {
	if r.URL.Path != "/auth/browser/token" {
		return false
	}
	clientOrigin, valid := secureBrowserOrigin(r.Header.Get("Origin"))
	if !valid || r.Header.Get("Access-Control-Request-Method") != http.MethodPost {
		forbidden(w)
		return true
	}
	for h := range strings.SplitSeq(r.Header.Get("Access-Control-Request-Headers"), ",") {
		if strings.ToLower(strings.TrimSpace(h)) != "content-type" && strings.TrimSpace(h) != "" {
			forbidden(w)
			return true
		}
	}
	cors.Bearer(w.Header(), clientOrigin)
	w.Header().Set("Access-Control-Allow-Methods", "POST")
	w.Header().Set("Access-Control-Allow-Headers", "Content-Type")
	w.WriteHeader(http.StatusNoContent)
	return true
}

func browserGrantRoute(path string) bool {
	_, ok := route.Lookup(path)
	return ok && path != route.Servers
}
