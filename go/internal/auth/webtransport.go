package auth

// A browser WebTransport CONNECT carries neither cookies nor headers, so the
// boundary accepts a session-linked token minted moments earlier over
// authenticated HTTP and carried in the CONNECT URL. Tokens are single-use,
// short-lived, capped per session, and revoked with their session, which the
// ambient-credential Origin/CSRF rules exist to approximate for cookies.

import (
	"context"
	"crypto/sha256"
	"net/http"
	"time"
)

const (
	wtTokenLifetime    = 30 * time.Second
	maxSessionWTTokens = 8
	wtTokenPrefix      = "gmw_"
)

type wtToken struct {
	sess    *session
	expires time.Time
}

// MintWebTransportToken mints a CONNECT token for the request's authenticated
// session. ok is false when the request carries no session-backed principal.
func (s *Service) MintWebTransportToken(r *http.Request) (token string, expires time.Time, ok bool) {
	p, hasPrincipal := PrincipalFromContext(r.Context())
	if !hasPrincipal || p.session == nil {
		return "", time.Time{}, false
	}
	raw, err := randomToken(32)
	if err != nil {
		return "", time.Time{}, false
	}
	token = wtTokenPrefix + raw
	h := sha256.Sum256([]byte(token))
	now := s.now()
	expires = now.Add(wtTokenLifetime)

	s.mu.Lock()
	defer s.mu.Unlock()
	if p.session.ctx.Err() != nil {
		return "", time.Time{}, false
	}
	if len(p.session.wtTokens) >= maxSessionWTTokens {
		var oldest [32]byte
		oldestExpiry := expires.Add(time.Minute)
		for th := range p.session.wtTokens {
			if t, live := s.wtTokens[th]; live && t.expires.Before(oldestExpiry) {
				oldest, oldestExpiry = th, t.expires
			}
		}
		delete(p.session.wtTokens, oldest)
		delete(s.wtTokens, oldest)
	}
	p.session.wtTokens[h] = struct{}{}
	s.wtTokens[h] = wtToken{sess: p.session, expires: expires}
	return token, expires, true
}

// consumeWebTransportToken authenticates and deletes one token: a captured URL
// is worthless once its CONNECT has landed.
func (s *Service) consumeWebTransportToken(raw string) (Principal, bool) {
	if raw == "" {
		return Principal{}, false
	}
	h := sha256.Sum256([]byte(raw))
	now := s.now()
	s.mu.Lock()
	defer s.mu.Unlock()
	t, ok := s.wtTokens[h]
	if !ok {
		return Principal{}, false
	}
	delete(s.wtTokens, h)
	delete(t.sess.wtTokens, h)
	if !now.Before(t.expires) || t.sess.ctx.Err() != nil {
		return Principal{}, false
	}
	sess := t.sess
	return Principal{Subject: sess.subject, Name: sess.name, Provider: sess.provider, Expires: sess.expires, session: sess, Bearer: true}, true
}

func (s *Service) expireWTTokensLocked(now time.Time) {
	for h, t := range s.wtTokens {
		if !now.Before(t.expires) {
			delete(s.wtTokens, h)
			delete(t.sess.wtTokens, h)
		}
	}
}

// isWebTransportRoute names the extended-CONNECT session routes. Pinned by
// api/routes.txt (routes_test.go).
func isWebTransportRoute(path string) bool {
	switch path {
	case "/wt/download", "/wt/upload", "/wt/ping":
		return true
	}
	return false
}

// serveWebTransportConnect authenticates an extended CONNECT via Authorization
// (native clients) or the single-use URL token (browsers), binds the principal
// and its session lifetime, and skips the ambient-credential origin rules: this
// credential is non-ambient, short-lived, and consumed on arrival.
func (s *Service) serveWebTransportConnect(w http.ResponseWriter, r *http.Request, next http.Handler, listener Listener, t trust) {
	if !t.Secure {
		s.writeAuthRequired(w, r, listener)
		return
	}
	p, ok := s.authenticate(r)
	if !ok {
		p, ok = s.consumeWebTransportToken(r.URL.Query().Get("token"))
	}
	if !ok || p.session == nil {
		s.writeAuthRequired(w, r, listener)
		return
	}
	// The session upgrade outlives this handler only until the adapter returns,
	// so ending the auth session unwinds the live WebTransport session.
	ctx, cancel := context.WithCancelCause(r.Context())
	stop := context.AfterFunc(p.session.ctx, func() { cancel(errSessionEnded) })
	defer func() { stop(); cancel(nil) }()
	next.ServeHTTP(w, r.WithContext(context.WithValue(ctx, principalKey{}, p)))
}
