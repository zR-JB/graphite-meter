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

// WTMint says why a mint produced no token. The two refusals need different
// answers: a session that holds no slot is intact and gets one back within
// wtTokenLifetime, so it must not be told its login is gone.
type WTMint int

const (
	WTMintOK WTMint = iota
	// WTMintNoSession is a request with nothing to bind a token to: no
	// principal, none with a session, or a native grant, which needs none.
	WTMintNoSession
	WTMintAtCapacity
)

// MintWebTransportSessionToken mints a CONNECT token for the request's
// authenticated session and classifies a refusal.
//
// A bearer grant is refused with the rest. It carries the very session its
// browser approval created, so a token minted from one occupies that login's
// cap, and nothing needs it to: a native CONNECT presents its Authorization
// header directly.
func (s *Service) MintWebTransportSessionToken(r *http.Request) (token string, expires time.Time, mint WTMint) {
	p, hasPrincipal := PrincipalFromContext(r.Context())
	if !hasPrincipal || p.session == nil || p.Bearer {
		return "", time.Time{}, WTMintNoSession
	}
	token = wtTokenPrefix + randomToken(32)
	h := sha256.Sum256([]byte(token))
	now := s.now()
	expires = now.Add(wtTokenLifetime)

	s.mu.Lock()
	defer s.mu.Unlock()
	if p.session.ctx.Err() != nil {
		return "", time.Time{}, WTMintNoSession
	}
	// Refuse at the cap rather than evicting: the oldest live token is one
	// another tab is about to present, and killing it turns one client's dial
	// into another's 403, whose remedy is a re-mint that evicts again.
	s.expireWTTokensLocked(now)
	if len(p.session.wtTokens) >= maxSessionWTTokens {
		return "", time.Time{}, WTMintAtCapacity
	}
	p.session.wtTokens[h] = struct{}{}
	s.wtTokens[h] = wtToken{sess: p.session, expires: expires}
	return token, expires, WTMintOK
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

// isWebTransportRoute names the extended-CONNECT session routes, the paths that
// take the non-ambient credential instead of the origin rules. Held to the
// "wt" rows of api/routes.txt by routes_test.go.
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
//
// Those two and nothing else. The session cookie is ambient, so it must not
// authenticate a CONNECT even if a browser attached one — which keeps the claim
// above true by construction rather than by browser behaviour.
func (s *Service) serveWebTransportConnect(w http.ResponseWriter, r *http.Request, next http.Handler, listener Listener, t trust) {
	if !t.Secure {
		s.writeAuthRequired(w, r, listener)
		return
	}
	// The token is consumed whichever credential wins, so one carried alongside
	// a valid grant is spent here rather than staying usable until its TTL.
	token := r.URL.Query().Get("token")
	p, ok := s.authenticateNonAmbient(r)
	if ok {
		if token != "" {
			s.consumeWebTransportToken(token) //nolint:errcheck // the grant already authenticated this CONNECT
		}
	} else {
		p, ok = s.consumeWebTransportToken(token)
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
