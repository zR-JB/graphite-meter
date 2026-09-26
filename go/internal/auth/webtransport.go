package auth

import (
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
	socketTokenLifetime    = 30 * time.Second
	maxSessionSocketTokens = 8
	socketTokenPrefix      = "gmw_"
)

// Socket tickets carry the authenticated principal, including its narrower grant lifetime.
type socketToken struct {
	principal      Principal
	target, origin string
	expires        time.Time
}

type SocketMint int

const (
	SocketMintOK SocketMint = iota
	SocketMintNoSession
	SocketMintAtCapacity
	SocketMintInvalidTarget
)

func (s *Service) MintSocketToken(r *http.Request, kind route.Kind) (string, time.Time, SocketMint) {
	p, ok := PrincipalFromContext(r.Context())
	if !ok || p.session == nil || p.Bearer && p.browserOrigin() == "" {
		return "", time.Time{}, SocketMintNoSession
	}
	target, err := url.Parse(r.URL.Query().Get("target"))
	if err != nil || target.User != nil || target.RawQuery != "" || target.ForceQuery || target.Fragment != "" {
		return "", time.Time{}, SocketMintInvalidTarget
	}
	spec, known := route.Lookup(target.Path)
	origin, err := wire.CanonicalOrigin(target.Scheme + "://" + target.Host)
	if err != nil || target.Scheme != "https" || !strings.EqualFold(target.Hostname(), s.public.Hostname()) ||
		!known || spec.Kind != kind {
		return "", time.Time{}, SocketMintInvalidTarget
	}
	token := socketTokenPrefix + randomToken(32)
	h := sha256.Sum256([]byte(token))
	now := time.Now()
	expires := now.Add(socketTokenLifetime)
	if p.session.expires.Before(expires) {
		expires = p.session.expires
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	if p.measurementContext().Err() != nil {
		return "", time.Time{}, SocketMintNoSession
	}
	s.expireSocketTokensLocked(now)
	held := 0
	for t := range maps.Values(s.socketTokens) {
		if t.principal.session == p.session {
			held++
		}
	}
	if held >= maxSessionSocketTokens {
		return "", time.Time{}, SocketMintAtCapacity
	}
	p.Bearer = true
	s.socketTokens[h] = socketToken{principal: p, target: origin + target.Path, origin: r.Header.Get("Origin"),
		expires: expires}
	return token, expires, SocketMintOK
}

func (s *Service) consumeSocketToken(raw string, r *http.Request) (Principal, bool) {
	if raw == "" {
		return Principal{}, false
	}
	h := sha256.Sum256([]byte(raw))
	now := time.Now()
	s.mu.Lock()
	defer s.mu.Unlock()
	t, ok := s.socketTokens[h]
	if !ok {
		return Principal{}, false
	}
	delete(s.socketTokens, h)
	origin, err := wire.CanonicalOrigin("https://" + r.Host)
	if err != nil || t.target != origin+r.URL.Path || t.origin != r.Header.Get("Origin") || !now.Before(t.expires) ||
		t.principal.measurementContext().Err() != nil {
		return Principal{}, false
	}
	return t.principal, true
}

func (s *Service) expireSocketTokensLocked(now time.Time) {
	maps.DeleteFunc(s.socketTokens, func(_ [32]byte, t socketToken) bool {
		return !now.Before(t.expires) || t.principal.measurementContext().Err() != nil
	})
}

func isWebTransportRoute(path string) bool {
	spec, ok := route.Lookup(path)
	return ok && spec.Kind == route.WebTransport
}
