package auth

// wrap.go is the enforcement boundary: nothing reaches a wrapped handler
// without a Principal, except the enumerated public auth routes and the
// headers-only CORS preflight.

import (
	"context"
	"crypto/sha256"
	"encoding/base64"
	"errors"
	"net/http"
	"strings"
	"time"
)

// Listener describes what a wrapped listener is allowed to serve. UI listeners
// carry the login surface; measurement-only listeners refuse it outright.
type Listener struct{ UI bool }

type principalKey struct{}

var errSessionEnded = errors.New("authentication session ended")

// Principal is the authenticated identity attached to a request context.
// Bearer principals come from a native-client grant and may only reach
// measurement routes.
type Principal struct {
	Subject, Name, Provider string
	Expires                 time.Time
	session                 *session
	Bearer                  bool
}

// Enforce applies the authentication boundary to next. It is installed outermost
// on every listener, so admission accounting, body reads, and WebSocket
// upgrades all happen behind it.
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

// serveAuthenticated handles a request that requires a principal: it demands
// TLS, authenticates, confines bearer grants to measurement routes, binds the
// principal (and its session-cancellation) to the request context, and enforces
// the cross-origin rules before dispatching. Enforce has already run the
// routing, preflight, and public-route guards.
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

// isPublicAuthRoute enumerates the routes reachable without a principal. Every
// other path on a wrapped listener requires one.
func (s *Service) isPublicAuthRoute(method, path string) bool {
	if method == http.MethodGet && (path == "/login" || path == "/auth/cli") || method == http.MethodPost && path == "/auth/cli/token" {
		return true
	}
	if (s.cfg.Mode == "password" || s.cfg.Mode == "hybrid") && method == http.MethodPost && path == "/auth/password" {
		return true
	}
	return (s.cfg.Mode == "oidc" || s.cfg.Mode == "hybrid") && (method == http.MethodPost && path == "/auth/oidc/start" || method == http.MethodGet && path == "/auth/oidc/callback")
}

// isMeasurementRoute enumerates the paths the boundary treats as measurement
// traffic: the routes pinned by api/routes.txt plus the discovery endpoint,
// which no route table carries. routes_test.go asserts the enumeration.
func isMeasurementRoute(path string) bool {
	switch path {
	case "/preflight", "/probe", "/download", "/upload/session", "/upload", "/upload/progress", "/ws/ping":
		return true
	}
	return false
}

// rotateSuppliedSession revokes the session named by the request's own session
// cookie, if it still keys a live one, as part of issuing sess. The password
// login POST is same-site, so the browser attaches the prior session cookie;
// the OIDC callback is cross-site and does not, so that path captures the prior
// session at /auth/oidc/start instead (see oidcTransaction.prior).
func (s *Service) rotateSuppliedSession(r *http.Request, sess *session) {
	if c, err := r.Cookie(sessionCookie); err == nil {
		s.revokeSessionHash(sha256.Sum256([]byte(c.Value)), sess)
	}
}

func (s *Service) authenticate(r *http.Request) (Principal, bool) {
	if raw := r.Header.Get("Authorization"); raw != "" {
		if !strings.HasPrefix(raw, "Bearer ") {
			return Principal{}, false
		}
		return s.authenticateGrant(strings.TrimPrefix(raw, "Bearer "))
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
	return Principal{Subject: sess.subject, Name: sess.name, Provider: sess.provider, Expires: sess.expires, session: sess}, true
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
		return Principal{Subject: sess.subject, Name: sess.name, Provider: "cli", Expires: sess.expires, session: sess, Bearer: true}, true
	}
	return Principal{}, false
}

// writeAuthRequired answers an unauthenticated request with the marker headers the
// browser and native clients key off, redirecting only the UI root.
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

// SessionEnded reports whether ctx was cancelled because the principal's
// session was revoked or expired mid-request.
func SessionEnded(ctx context.Context) bool {
	return errors.Is(context.Cause(ctx), errSessionEnded)
}
