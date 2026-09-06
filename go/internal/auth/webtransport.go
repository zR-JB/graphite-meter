package auth

import (
	"context"
	"crypto/sha256"
	"maps"
	"net/http"
	"net/url"
	"strings"
	"time"

	"github.com/zR-JB/graphite-meter/go/internal/route"
	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

const (
	wtTokenLifetime    = 30 * time.Second
	maxSessionWTTokens = 8
	wtTokenPrefix      = "gmw_"
)

// Socket tickets carry the authenticated principal, including its narrower grant lifetime.
type wtToken struct {
	sess           *session
	principal      Principal
	target, origin string
	expires        time.Time
}

type WTMint int

const (
	WTMintOK WTMint = iota
	WTMintNoSession
	WTMintAtCapacity
	WTMintInvalidTarget
)

func (s *Service) MintWebTransportSessionToken(r *http.Request) (string, time.Time, WTMint) {
	return s.mintSocketToken(r, route.WebTransport)
}

func (s *Service) MintWebSocketSessionToken(r *http.Request) (string, time.Time, WTMint) {
	return s.mintSocketToken(r, route.WebSocket)
}

func (s *Service) mintSocketToken(r *http.Request, kind route.Kind) (string, time.Time, WTMint) {
	p, ok := PrincipalFromContext(r.Context())
	if !ok || p.session == nil || p.Bearer && p.browserGrant == nil {
		return "", time.Time{}, WTMintNoSession
	}
	target, err := url.Parse(r.URL.Query().Get("target"))
	if err != nil || target.User != nil || target.RawQuery != "" || target.ForceQuery || target.Fragment != "" {
		return "", time.Time{}, WTMintInvalidTarget
	}
	spec, known := route.Lookup(target.Path)
	origin, err := wire.CanonicalOrigin(target.Scheme + "://" + target.Host)
	if err != nil || target.Scheme != "https" || !strings.EqualFold(target.Hostname(), s.public.Hostname()) || !known || spec.Kind != kind {
		return "", time.Time{}, WTMintInvalidTarget
	}
	token := wtTokenPrefix + randomToken(32)
	h := sha256.Sum256([]byte(token))
	now := s.now()
	expires := minTime(now.Add(wtTokenLifetime), p.session.expires)
	s.mu.Lock()
	defer s.mu.Unlock()
	if p.measurementContext().Err() != nil {
		return "", time.Time{}, WTMintNoSession
	}
	s.expireWTTokensLocked(now)
	if len(p.session.wtTokens) >= maxSessionWTTokens {
		return "", time.Time{}, WTMintAtCapacity
	}
	p.Bearer = true
	p.session.wtTokens[h] = struct{}{}
	s.wtTokens[h] = wtToken{sess: p.session, principal: p, target: origin + target.Path, origin: r.Header.Get("Origin"), expires: expires}
	return token, expires, WTMintOK
}

func minTime(a, b time.Time) time.Time {
	if a.Before(b) {
		return a
	}
	return b
}

func (s *Service) consumeWebTransportToken(raw string, r *http.Request) (Principal, bool) {
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
	origin, err := wire.CanonicalOrigin("https://" + r.Host)
	if err != nil || t.target != origin+r.URL.Path || t.origin != r.Header.Get("Origin") || !now.Before(t.expires) || t.principal.measurementContext().Err() != nil {
		return Principal{}, false
	}
	return t.principal, true
}

func (s *Service) expireWTTokensLocked(now time.Time) {
	maps.DeleteFunc(s.wtTokens, func(h [32]byte, t wtToken) bool {
		if now.Before(t.expires) && t.principal.measurementContext().Err() == nil {
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
			s.consumeWebTransportToken(token, r)
		}
	} else {
		p, ok = s.consumeWebTransportToken(token, r)
	}
	if !ok || p.session == nil || !s.validRequestOrigin(r, p) {
		s.writeAuthRequired(w, r, listener)
		return
	}
	ctx, cancel := context.WithCancelCause(r.Context())
	stop := context.AfterFunc(p.measurementContext(), func() { cancel(errSessionEnded) })
	defer func() { stop(); cancel(nil) }()
	next.ServeHTTP(w, r.WithContext(context.WithValue(ctx, principalKey{}, p)))
}
