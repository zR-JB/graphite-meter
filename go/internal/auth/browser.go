package auth

import (
	"context"
	"net/http"
	"net/url"
	"strings"
	"time"

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
	client, ok := s.approvalClient(r)
	if !ok {
		forbidden(w)
		return
	}
	now := time.Now()
	s.mu.Lock()
	a := s.approvals[challenge]
	s.mu.Unlock()
	if a == nil && !s.allowBrowserApproval(r) {
		forbidden(w)
		return
	}
	s.mu.Lock()
	_, clientRoom := s.approvalRoomLocked(nil, client, now)
	a = s.approvals[challenge]
	if a == nil && clientRoom {
		a = &cliApproval{code: verificationCode(challenge), expires: now.Add(approvalLifetime),
			browserOrigin: clientOrigin, client: client}
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
		if r.Header.Get("Sec-Fetch-Site") == "cross-site" && r.Header.Get("Sec-Fetch-Mode") == "navigate" &&
			r.Header.Get("Sec-Fetch-Dest") == "document" {
			// A document navigation makes the Strict session cookie available on
			// reentry. An HTTP redirect would retain the cross-site cookie context.
			render(w, continueTemplate, map[string]any{"Styles": authStyles, "Challenge": challenge, "Opening": true})
			return
		}
		loginRedirect(w, r, challenge)
		return
	}
	s.mu.Lock()
	sessionRoom, _ := s.approvalRoomLocked(p.session, client, now)
	valid = s.approvals[challenge] == a && (a.session == p.session || a.session == nil && sessionRoom)
	if valid {
		a.session = p.session
	}
	atCapacity := valid && len(p.session.grants) >= maxSessionGrants
	s.mu.Unlock()
	switch {
	case !valid:
		forbidden(w)
	case atCapacity:
		writeBrowserGrantCapacity(w, clientOrigin)
	default:
		render(w, cliTemplate, map[string]any{"Styles": authStyles, "Code": a.code, "Challenge": challenge,
			"CSRF": p.session.csrf, "BrowserOrigin": clientOrigin})
	}
}

func writeBrowserGrantCapacity(w http.ResponseWriter, clientOrigin string) {
	w.Header().Set("Content-Type", "text/html; charset=utf-8")
	w.WriteHeader(http.StatusTooManyRequests)
	_ = cliTemplate.Execute(w, map[string]any{
		"Styles": authStyles, "BrowserOrigin": clientOrigin, "BrowserCapacity": true, "ClientLimit": maxSessionGrants,
	})
}

func (s *Service) browserToken(w http.ResponseWriter, r *http.Request) {
	securityHeaders(w.Header())
	clientOrigin, valid := secureBrowserOrigin(r.Header.Get("Origin"))
	if !valid {
		forbidden(w)
		return
	}
	bearerCORS(w.Header(), clientOrigin)
	verifier, ok := readVerifier(w, r)
	if !ok || len(verifier) < 32 {
		forbidden(w)
		return
	}
	now := time.Now()
	s.mu.Lock()
	challenge, a := s.approvalLocked(verifier, now)
	if a == nil || a.browserOrigin != clientOrigin {
		s.mu.Unlock()
		writeGrantPending(w)
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
		writeGrantPending(w)
		return
	}
	raw, key := s.issueGrantLocked(challenge, sess)
	ctx, cancel := context.WithCancel(sess.ctx)
	s.browserGrants[key] = &browserGrant{sess: sess, origin: clientOrigin, id: randomToken(16),
		ctx: ctx, cancel: cancel}
	s.mu.Unlock()
	writeJSON(w, map[string]any{"token": raw, "expires": sess.expires.UnixMilli(),
		"remainingMs": sess.expires.Sub(now).Milliseconds(), "maximumLifetimeMs": sessionLifetime.Milliseconds()})
}

func (s *Service) browserApprovalRedirect(challenge string) string {
	s.mu.Lock()
	defer s.mu.Unlock()
	if a := s.approvals[challenge]; a != nil && a.browserOrigin != "" && time.Now().Before(a.expires) {
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
	p, _ := PrincipalFromContext(r.Context())
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
	if _, allowed := requestedHeaders(r, "content-type"); !valid || !allowed ||
		r.Header.Get("Access-Control-Request-Method") != http.MethodPost {
		forbidden(w)
		return true
	}
	bearerCORS(w.Header(), clientOrigin)
	w.Header().Set("Access-Control-Allow-Methods", "POST")
	w.Header().Set("Access-Control-Allow-Headers", "Content-Type")
	w.WriteHeader(http.StatusNoContent)
	return true
}

func browserGrantRoute(path string) bool {
	_, ok := route.Lookup(path)
	return ok && path != route.Servers
}
