package auth

import (
	"crypto/sha256"
	"encoding/json/v2"
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

// SocketTokenHandler serves /wt/session or /ws/session; public mode answers an empty token.
func (s *Service) SocketTokenHandler(kind route.Kind) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		var response struct {
			Token   string `json:"token"`
			Expires int64  `json:"expires"`
		}
		if s.Enabled() {
			token, expires, status := s.mintSocketToken(r, kind)
			if status != http.StatusOK {
				// Capacity, not permission: the login is intact and its oldest ticket expires soon.
				if status == http.StatusTooManyRequests {
					w.Header().Set("Retry-After", "1")
				}
				http.Error(w, http.StatusText(status), status)
				return
			}
			response.Token, response.Expires = token, expires.UnixMilli()
		}
		w.Header().Set("Content-Type", "application/json")
		w.Header().Set("Cache-Control", "no-store")
		_ = json.MarshalWrite(w, response)
	})
}

func (s *Service) mintSocketToken(r *http.Request, kind route.Kind) (string, time.Time, int) {
	p, ok := PrincipalFromContext(r.Context())
	if !ok || p.session == nil || p.Bearer && p.browserOrigin() == "" {
		return "", time.Time{}, http.StatusForbidden
	}
	target, err := url.Parse(r.URL.Query().Get("target"))
	if err != nil || target.User != nil || target.RawQuery != "" || target.ForceQuery || target.Fragment != "" {
		return "", time.Time{}, http.StatusBadRequest
	}
	spec, known := route.Lookup(target.Path)
	origin, err := wire.CanonicalOrigin(target.Scheme + "://" + target.Host)
	if err != nil || target.Scheme != "https" || !strings.EqualFold(target.Hostname(), s.public.Hostname()) ||
		!known || spec.Kind != kind {
		return "", time.Time{}, http.StatusBadRequest
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
		return "", time.Time{}, http.StatusForbidden
	}
	s.expireSocketTokensLocked(now)
	held := 0
	for t := range maps.Values(s.socketTokens) {
		if t.principal.session == p.session {
			held++
		}
	}
	if held >= maxSessionSocketTokens {
		return "", time.Time{}, http.StatusTooManyRequests
	}
	p.Bearer = true
	s.socketTokens[h] = socketToken{principal: p, target: origin + target.Path, origin: r.Header.Get("Origin"),
		expires: expires}
	return token, expires, http.StatusOK
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
