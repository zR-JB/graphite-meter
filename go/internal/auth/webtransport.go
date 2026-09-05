package auth

import (
	"context"
	"crypto/sha256"
	"maps"
	"net/http"
	"time"

	"github.com/zR-JB/graphite-meter/go/internal/route"
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

type WTMint int

const (
	WTMintOK WTMint = iota
	WTMintNoSession
	WTMintAtCapacity
)

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
	s.expireWTTokensLocked(now)
	if len(p.session.wtTokens) >= maxSessionWTTokens {
		return "", time.Time{}, WTMintAtCapacity
	}
	p.session.wtTokens[h] = struct{}{}
	s.wtTokens[h] = wtToken{sess: p.session, expires: expires}
	return token, expires, WTMintOK
}

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
	return sessionPrincipal(sess, sess.provider, true), true
}

func (s *Service) expireWTTokensLocked(now time.Time) {
	maps.DeleteFunc(s.wtTokens, func(h [32]byte, t wtToken) bool {
		if now.Before(t.expires) {
			return false
		}
		delete(t.sess.wtTokens, h)
		return true
	})
}

func isWebTransportRoute(path string) bool {
	spec, ok := route.Lookup(path)
	return ok && spec.Kind == route.WebTransport
}

func (s *Service) serveWebTransportConnect(w http.ResponseWriter, r *http.Request, next http.Handler, listener Listener, t trust) {
	if !t.Secure {
		s.writeAuthRequired(w, r, listener)
		return
	}
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
	ctx, cancel := context.WithCancelCause(r.Context())
	stop := context.AfterFunc(p.session.ctx, func() { cancel(errSessionEnded) })
	defer func() { stop(); cancel(nil) }()
	next.ServeHTTP(w, r.WithContext(context.WithValue(ctx, principalKey{}, p)))
}
