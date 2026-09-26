package auth

import (
	"context"
	"crypto/sha256"
	"encoding/base64"
	"errors"
	"net/http"
	"strings"
	"time"

	"github.com/zR-JB/graphite-meter/go/internal/route"
)

type Listener struct{ UI, WebTransport bool }

type principalKey struct{}

var errSessionEnded = errors.New("authentication session ended")

type Principal struct {
	Subject, Name, Provider string
	Expires                 time.Time
	Bearer                  bool
	session                 *session
	grant                   *grant
}

func sessionPrincipal(sess *session, provider string, bearer bool) Principal {
	return Principal{Subject: sess.subject, Name: sess.name, Provider: provider, Expires: sess.expires,
		session: sess, Bearer: bearer}
}

func (s *Service) Enforce(next http.Handler, listener Listener) http.Handler {
	if !s.Enabled() {
		return next
	}
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if ambiguousAuthHeaders(r.Header) {
			forbidden(w)
			return
		}
		t := s.requestTrust(r)
		if t.Secure && r.TLS != nil && !strings.EqualFold(requestHostname(r.Host), s.public.Hostname()) {
			s.writeAuthRequired(w, r, listener)
			return
		}
		if t.Secure {
			w.Header().Set("Strict-Transport-Security", hstsThisHostOnly)
			HardeningHeaders(w.Header())
		}
		if r.Method == http.MethodOptions && (isMeasurementRoute(r.URL.Path) || r.URL.Path == "/auth/browser/token") {
			s.corsPreflight(w, r, t)
			return
		}
		if (r.URL.Path == "/login" || strings.HasPrefix(r.URL.Path, "/auth/")) && (!listener.UI || !t.Canonical) {
			forbidden(w)
			return
		}
		if listener.UI && s.isPublicAuthRoute(r.Method, r.URL.Path) {
			if !t.Secure || !t.Canonical {
				s.writeAuthRequired(w, r, listener)
				return
			}
			next.ServeHTTP(w, r)
			return
		}
		s.serveAuthenticated(w, r, next, listener, t)
	})
}

func (s *Service) serveAuthenticated(w http.ResponseWriter, r *http.Request, next http.Handler, listener Listener,
	t trust) {
	var p Principal
	ok := t.Secure
	if ok && r.Method == http.MethodConnect && listener.WebTransport && isWebTransportRoute(r.URL.Path) {
		// A CONNECT never uses the cookie and always spends its ticket.
		ticket, spent := s.consumeSocketToken(r.URL.Query().Get("token"), r)
		if p, ok = s.authenticateBearer(r); !ok {
			p, ok = ticket, spent
		}
	} else if ok {
		p, ok = s.authenticate(r)
	}
	if !ok {
		s.writeAuthRequired(w, r, listener)
		return
	}
	if p.Bearer && (!isMeasurementRoute(r.URL.Path) || p.browserOrigin() != "" && !browserGrantRoute(r.URL.Path)) ||
		!s.validRequestOrigin(r, p) {
		forbidden(w)
		return
	}
	r, end := withPrincipal(r, p)
	defer end()
	next.ServeHTTP(w, r)
}

func (s *Service) isPublicAuthRoute(method, path string) bool {
	password, oidc := authModes(s.cfg.Mode)
	switch method + " " + path {
	case "GET /login", "GET /auth/cli", "GET /auth/browser", "POST /auth/cli/token", "POST /auth/browser/token":
		return true
	case "POST /auth/password":
		return password
	case "POST /auth/oidc/start", "GET /auth/oidc/callback":
		return oidc
	}
	return false
}

func isMeasurementRoute(path string) bool {
	_, ok := route.Lookup(path)
	return ok
}

func (s *Service) rotateSuppliedSession(r *http.Request, sess *session) {
	if c := uniqueCookie(r, sessionCookie); c != nil {
		s.revokeSessionHash(sha256.Sum256([]byte(c.Value)), sess)
	}
}

func (s *Service) authenticate(r *http.Request) (Principal, bool) {
	if spec, ok := route.Lookup(r.URL.Path); ok && spec.Kind == route.WebSocket {
		if query := r.URL.Query(); query.Has("token") {
			return s.consumeSocketToken(query.Get("token"), r)
		}
	}
	if len(r.Header.Values("Authorization")) != 0 {
		return s.authenticateBearer(r)
	}
	c := uniqueCookie(r, sessionCookie)
	if c == nil {
		return Principal{}, false
	}
	h := sha256.Sum256([]byte(c.Value))
	now := time.Now()
	s.mu.Lock()
	defer s.mu.Unlock()
	sess, ok := s.sessions[h]
	if !ok || !now.Before(sess.expires) {
		if ok {
			s.deleteSessionLocked(sess)
		}
		return Principal{}, false
	}
	return sessionPrincipal(sess, sess.provider, false), true
}

func (s *Service) authenticateBearer(r *http.Request) (Principal, bool) {
	values := r.Header.Values("Authorization")
	if len(values) != 1 {
		return Principal{}, false
	}
	raw, ok := strings.CutPrefix(values[0], "Bearer ")
	if !ok {
		return Principal{}, false
	}
	return s.authenticateGrant(raw)
}

func (s *Service) authenticateGrant(raw string) (Principal, bool) {
	decoded, err := base64.RawURLEncoding.DecodeString(raw)
	if err != nil || len(decoded) != 32 || base64.RawURLEncoding.EncodeToString(decoded) != raw {
		return Principal{}, false
	}
	h := sha256.Sum256([]byte(raw))
	now := time.Now()
	s.mu.Lock()
	defer s.mu.Unlock()
	g := s.grants[h]
	if g == nil || !now.Before(g.sess.expires) || g.ctx.Err() != nil {
		return Principal{}, false
	}
	provider := "cli"
	if g.origin != "" {
		provider = "browser"
	}
	p := sessionPrincipal(g.sess, provider, true)
	p.grant = g
	return p, true
}

func (s *Service) writeAuthRequired(w http.ResponseWriter, r *http.Request, listener Listener) {
	securityHeaders(w.Header())
	s.MeasurementCORS(w.Header(), r)
	w.Header().Set("Graphite-Meter-Auth", "required")
	w.Header().Set("Graphite-Meter-Browser-Auth", "1")
	w.Header().Set("Graphite-Meter-Auth-URL", s.origin+"/login")
	if r.ProtoMajor == 1 && r.Body != nil {
		w.Header().Set("Connection", "close")
	}
	if listener.UI && r.Method == http.MethodGet && r.URL.Path == "/" {
		s.debugln("unauthenticated UI root redirected to login")
		http.Redirect(w, r, s.origin+"/login", http.StatusTemporaryRedirect)
		return
	}
	w.WriteHeader(http.StatusForbidden)
}

func forbidden(w http.ResponseWriter) {
	securityHeaders(w.Header())
	w.WriteHeader(http.StatusForbidden)
}

// withPrincipal carries p on r's context, ending it with errSessionEnded when p's login or grant does.
func withPrincipal(r *http.Request, p Principal) (*http.Request, func()) {
	if p.session == nil {
		return r.WithContext(context.WithValue(r.Context(), principalKey{}, p)), func() {}
	}
	ctx, cancel := context.WithCancelCause(r.Context())
	stop := context.AfterFunc(p.measurementContext(), func() { cancel(errSessionEnded) })
	return r.WithContext(context.WithValue(ctx, principalKey{}, p)), func() { stop(); cancel(nil) }
}

func PrincipalFromContext(ctx context.Context) (Principal, bool) {
	p, ok := ctx.Value(principalKey{}).(Principal)
	return p, ok
}

func (s *Service) sessionFormPrincipal(r *http.Request) (Principal, bool) {
	p, ok := PrincipalFromContext(r.Context())
	return p, ok && p.session != nil &&
		r.Header.Get("Origin") == s.origin && constantEqual(p.session.csrf, r.FormValue("csrf"))
}

func SessionEnded(ctx context.Context) bool {
	return errors.Is(context.Cause(ctx), errSessionEnded)
}
