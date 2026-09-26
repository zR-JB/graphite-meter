package auth

import (
	"context"
	"crypto/sha256"
	"encoding/base32"
	"encoding/base64"
	"encoding/json/v2"
	"html/template"
	"maps"
	"net/http"
	"net/url"
	"strings"
	"time"

	"github.com/zR-JB/graphite-meter/go/internal/route"
	"github.com/zR-JB/graphite-meter/go/internal/transport"
	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

const (
	maxApprovals        = 256
	maxSessionApprovals = 8
	maxClientApprovals  = 8
	maxSessionGrants    = 8
	approvalLifetime    = 2 * time.Minute
)

// approval is a pending delegation of a login to a native client, or to one browser origin.
type approval struct {
	browserOrigin string
	client        string
	code          string
	session       *session
	expires       time.Time
	approved      bool
}

// grant is the bearer credential an approval becomes; it ends with its login or on eviction.
type grant struct {
	sess   *session
	key    [32]byte
	origin string // the browser origin it serves, or "" for a native client
	id     string
	seq    uint64
	ctx    context.Context
	cancel context.CancelFunc
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
	value, _ := base64.RawURLEncoding.DecodeString(challenge)
	return base32.StdEncoding.WithPadding(base32.NoPadding).EncodeToString(value[:5])
}

func secureBrowserOrigin(raw string) (string, bool) {
	canonical, err := wire.CanonicalOrigin(raw)
	return canonical, err == nil && strings.HasPrefix(canonical, "https://") && raw == canonical
}

func browserGrantRoute(path string) bool {
	_, ok := route.Lookup(path)
	return ok && path != route.Servers
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

func (s *Service) approvalRoomLocked(sess *session, client string, now time.Time) (bool, bool) {
	maps.DeleteFunc(s.approvals, func(_ string, a *approval) bool { return !now.Before(a.expires) })
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
	s.mu.Lock()
	a := s.approvals[challenge]
	s.mu.Unlock()
	if a != nil && a.browserOrigin != "" && time.Now().Before(a.expires) {
		query := url.Values{"challenge": {challenge}, "client_origin": {a.browserOrigin}}
		http.Redirect(w, r, "/auth/browser?"+query.Encode(), http.StatusSeeOther)
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
	a = s.approvals[challenge]
	if a == nil && sessionRoom && clientRoom {
		a = &approval{code: verificationCode(challenge), session: p.session, client: client,
			expires: now.Add(approvalLifetime)}
		s.approvals[challenge] = a
	}
	s.mu.Unlock()
	if a == nil {
		s.count(countCapacity)
		forbidden(w)
		return
	}
	render(w, cliTemplate,
		map[string]any{"Styles": authStyles, "Code": a.code, "Challenge": challenge, "CSRF": p.session.csrf})
}

func (s *Service) browserPage(w http.ResponseWriter, r *http.Request) {
	securityHeaders(w.Header())
	challenge := r.URL.Query().Get("challenge")
	clientOrigin, valid := secureBrowserOrigin(r.URL.Query().Get("client_origin"))
	client, ok := s.approvalClient(r)
	if !validChallenge(challenge) || !valid || !ok {
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
		a = &approval{code: verificationCode(challenge), expires: now.Add(approvalLifetime),
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

func (s *Service) approve(w http.ResponseWriter, r *http.Request) {
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
	a := s.approvals[r.FormValue("challenge")]
	if a == nil || a.session != p.session || (a.browserOrigin != "") != browser || !time.Now().Before(a.expires) {
		s.mu.Unlock()
		forbidden(w)
		return
	}
	if browser && len(p.session.grants) >= maxSessionGrants {
		s.mu.Unlock()
		writeBrowserGrantCapacity(w, a.browserOrigin)
		return
	}
	a.approved = true
	s.mu.Unlock()
	s.count(countCLIApproval)
	render(w, cliDoneTemplate, map[string]any{"Styles": authStyles, "Browser": browser})
}

// token exchanges an approval's verifier for a grant. A browser exchange answers only its approved origin,
// cookie-free; a native exchange at capacity replaces the oldest native grant, never a browser one.
func (s *Service) token(w http.ResponseWriter, r *http.Request) {
	securityHeaders(w.Header())
	origin, browser := "", r.URL.Path == "/auth/browser/token"
	if browser {
		var valid bool
		if origin, valid = secureBrowserOrigin(r.Header.Get("Origin")); !valid {
			forbidden(w)
			return
		}
		bearerCORS(w.Header(), origin)
	}
	r.Body = http.MaxBytesReader(w, r.Body, 4096)
	var req struct {
		Verifier string `json:"verifier"`
	}
	if err := json.UnmarshalRead(r.Body, &req); err != nil || len(req.Verifier) > 128 || browser &&
		len(req.Verifier) < 32 {
		if browser {
			forbidden(w)
		} else {
			writeGrantPending(w)
		}
		return
	}
	now := time.Now()
	s.mu.Lock()
	raw, sess, status := s.exchangeLocked(req.Verifier, origin, now)
	s.mu.Unlock()
	switch {
	case status == http.StatusAccepted:
		writeGrantPending(w)
	case status != http.StatusOK:
		w.WriteHeader(status)
	case !browser:
		writeJSON(w, map[string]any{"token": raw, "expires": sess.expires})
	default:
		writeJSON(w, map[string]any{"token": raw, "expires": sess.expires.UnixMilli(),
			"remainingMs": sess.expires.Sub(now).Milliseconds(), "maximumLifetimeMs": sessionLifetime.Milliseconds()})
	}
}

func (s *Service) exchangeLocked(verifier, origin string, now time.Time) (string, *session, int) {
	sum := sha256.Sum256([]byte(verifier))
	challenge := base64.RawURLEncoding.EncodeToString(sum[:])
	a := s.approvals[challenge]
	if a == nil || a.browserOrigin != origin || a.session == nil || !now.Before(a.expires) ||
		!now.Before(a.session.expires) || a.session.ctx.Err() != nil || origin == "" && !a.approved {
		return "", nil, http.StatusAccepted
	}
	sess := a.session
	if len(sess.grants) >= maxSessionGrants {
		var oldest *grant
		for g := range maps.Values(sess.grants) {
			if origin == "" && g.origin == "" && (oldest == nil || g.seq < oldest.seq) {
				oldest = g
			}
		}
		if oldest == nil {
			return "", nil, http.StatusTooManyRequests
		}
		s.deleteGrantLocked(oldest)
	}
	if !a.approved {
		return "", nil, http.StatusAccepted
	}
	delete(s.approvals, challenge)
	raw := randomToken(32)
	ctx, cancel := context.WithCancel(sess.ctx)
	s.grantSeq++
	g := &grant{sess: sess, key: sha256.Sum256([]byte(raw)), origin: origin, id: randomToken(16), seq: s.grantSeq,
		ctx: ctx, cancel: cancel}
	s.grants[g.key], sess.grants[g.key] = g, g
	return raw, sess, http.StatusOK
}

func writeGrantPending(w http.ResponseWriter) {
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(http.StatusAccepted)
	_, _ = w.Write([]byte(`{"status":"pending"}`))
}

func (s *Service) deleteGrantLocked(g *grant) {
	delete(g.sess.grants, g.key)
	delete(s.grants, g.key)
	g.cancel()
}

// BrowserOrigin is supplied only after a browser grant or its socket ticket authenticated.
func BrowserOrigin(r *http.Request) string {
	p, _ := PrincipalFromContext(r.Context())
	return p.browserOrigin()
}

func (p Principal) browserOrigin() string {
	if p.grant == nil {
		return ""
	}
	return p.grant.origin
}

func (p Principal) measurementContext() context.Context {
	if p.grant != nil {
		return p.grant.ctx
	}
	return p.session.ctx
}

func (p Principal) MeasurementOwner() string {
	if p.browserOrigin() == "" {
		return ""
	}
	return "principal:" + p.Subject + "\x00browser-grant:" + p.grant.id
}
