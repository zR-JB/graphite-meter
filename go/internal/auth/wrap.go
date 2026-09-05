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
	session                 *session
	Bearer                  bool
}

func sessionPrincipal(sess *session, provider string, bearer bool) Principal {
	return Principal{Subject: sess.subject, Name: sess.name, Provider: provider, Expires: sess.expires, session: sess, Bearer: bearer}
}

func (p Principal) LoginID() string {
	if p.session == nil {
		return ""
	}
	return p.session.id
}

func (s *Service) Enforce(next http.Handler, listener Listener) http.Handler {
	if !s.Enabled() {
		return next
	}
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		t := s.requestTrust(r)
		if t.Secure && r.TLS != nil && !strings.EqualFold(requestHostname(r.Host), s.public.Hostname()) {
			s.writeAuthRequired(w, r, listener)
			return
		}
		if t.Secure {
			s.authenticatedSecurityHeaders(w.Header())
		}
		if r.Method == http.MethodPost && strings.HasPrefix(r.URL.Path, "/auth/") {
			controller := http.NewResponseController(w)
			_ = controller.SetReadDeadline(s.now().Add(15 * time.Second))
			defer controller.SetReadDeadline(time.Time{})
		}
		if r.Method == http.MethodOptions && isMeasurementRoute(r.URL.Path) {
			s.corsPreflight(w, r, t.Secure)
			return
		}
		if r.Method == http.MethodConnect && listener.WebTransport && isWebTransportRoute(r.URL.Path) {
			s.serveWebTransportConnect(w, r, next, listener, t)
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

func (s *Service) serveAuthenticated(w http.ResponseWriter, r *http.Request, next http.Handler, listener Listener, t trust) {
	if !t.Secure {
		s.writeAuthRequired(w, r, listener)
		return
	}
	p, ok := s.authenticate(r)
	if !ok {
		s.writeAuthRequired(w, r, listener)
		return
	}
	if p.Bearer && !isMeasurementRoute(r.URL.Path) {
		forbidden(w)
		return
	}
	if p.session != nil {
		ctx, cancel := context.WithCancelCause(r.Context())
		stop := context.AfterFunc(p.session.ctx, func() { cancel(errSessionEnded) })
		defer func() { stop(); cancel(nil) }()
		r = r.WithContext(context.WithValue(ctx, principalKey{}, p))
	} else {
		r = r.WithContext(context.WithValue(r.Context(), principalKey{}, p))
	}
	if !s.validRequestOrigin(r, p) {
		forbidden(w)
		return
	}
	next.ServeHTTP(w, r)
}

func (s *Service) isPublicAuthRoute(method, path string) bool {
	if method == http.MethodGet && (path == "/login" || path == "/auth/cli") || method == http.MethodPost && path == "/auth/cli/token" {
		return true
	}
	password, oidc := authModes(s.cfg.Mode)
	if password && method == http.MethodPost && path == "/auth/password" {
		return true
	}
	return oidc && (method == http.MethodPost && path == "/auth/oidc/start" || method == http.MethodGet && path == "/auth/oidc/callback")
}

func isMeasurementRoute(path string) bool {
	_, ok := route.Lookup(path)
	return ok
}

func (s *Service) rotateSuppliedSession(r *http.Request, sess *session) {
	if c, err := r.Cookie(sessionCookie); err == nil {
		s.revokeSessionHash(sha256.Sum256([]byte(c.Value)), sess)
	}
}

func (s *Service) authenticate(r *http.Request) (Principal, bool) {
	if r.Header.Get("Authorization") != "" {
		return s.authenticateNonAmbient(r)
	}
	c, err := r.Cookie(sessionCookie)
	if err != nil {
		return Principal{}, false
	}
	h := sha256.Sum256([]byte(c.Value))
	now := s.now()
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

func (s *Service) authenticateNonAmbient(r *http.Request) (Principal, bool) {
	raw := r.Header.Get("Authorization")
	raw, ok := strings.CutPrefix(raw, "Bearer ")
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
	now := s.now()
	s.mu.Lock()
	defer s.mu.Unlock()
	sess := s.grants[h]
	if sess != nil && now.Before(sess.expires) && sess.ctx.Err() == nil {
		return sessionPrincipal(sess, "cli", true), true
	}
	return Principal{}, false
}

func (s *Service) writeAuthRequired(w http.ResponseWriter, r *http.Request, listener Listener) {
	securityHeaders(w.Header())
	if s.public != nil && r.Header.Get("Origin") == s.public.String() {
		h := w.Header()
		h.Set("Access-Control-Allow-Origin", s.public.String())
		h.Set("Access-Control-Allow-Credentials", "true")
		h.Set("Access-Control-Expose-Headers", "Graphite-Meter-Auth, Graphite-Meter-Auth-URL")
		h.Set("Timing-Allow-Origin", s.public.String())
		h.Add("Vary", "Origin")
	}
	w.Header().Set("Graphite-Meter-Auth", "required")
	w.Header().Set("Graphite-Meter-Auth-URL", s.public.String()+"/login")
	if r.ProtoMajor == 1 && r.Body != nil {
		w.Header().Set("Connection", "close")
	}
	if listener.UI && r.Method == http.MethodGet && r.URL.Path == "/" {
		s.debugln("unauthenticated UI root redirected to login")
		http.Redirect(w, r, s.public.String()+"/login", http.StatusTemporaryRedirect)
		return
	}
	w.WriteHeader(http.StatusForbidden)
}

func forbidden(w http.ResponseWriter) {
	securityHeaders(w.Header())
	w.WriteHeader(http.StatusForbidden)
}

func PrincipalFromContext(ctx context.Context) (Principal, bool) {
	p, ok := ctx.Value(principalKey{}).(Principal)
	return p, ok
}

func (s *Service) sessionFormPrincipal(r *http.Request) (Principal, bool) {
	p, ok := PrincipalFromContext(r.Context())
	return p, ok && p.session != nil &&
		r.Header.Get("Origin") == s.public.String() && constantEqual(p.session.csrf, r.FormValue("csrf"))
}

func SessionEnded(ctx context.Context) bool {
	return errors.Is(context.Cause(ctx), errSessionEnded)
}
