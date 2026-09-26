package auth

import (
	"crypto/subtle"
	"net"
	"net/http"
	"net/netip"
	"strings"

	"github.com/zR-JB/graphite-meter/go/internal/route"
	"github.com/zR-JB/graphite-meter/go/internal/transport"
)

type trust struct{ Secure, Canonical bool }

// Security-relevant fields must appear at most once per request.
func ambiguousAuthHeaders(h http.Header) bool {
	for _, name := range [...]string{
		"Authorization", "Origin", "Sec-Fetch-Site", "X-CSRF-Token",
		"Access-Control-Request-Method", "Access-Control-Request-Headers",
	} {
		if len(h.Values(name)) > 1 {
			return true
		}
	}
	return false
}

func (s *Service) requestTrust(r *http.Request) trust {
	if r.TLS != nil {
		return trust{Secure: true, Canonical: equalHost(r.Host, s.public.Host)}
	}
	if peer, ok := transport.Peer(r.RemoteAddr); !ok || !transport.Trusted(peer, s.trusted) {
		return trust{}
	}
	proto := singleHeader(r.Header, "X-Forwarded-Proto")
	host := singleHeader(r.Header, "X-Forwarded-Host")
	forwarded := proto == "https" && equalHost(host, s.public.Host)
	return trust{Secure: forwarded, Canonical: forwarded}
}

func singleHeader(h http.Header, name string) string {
	v := h.Values(name)
	if len(v) != 1 || strings.Contains(v[0], ",") {
		return ""
	}
	return strings.TrimSpace(v[0])
}

func equalHost(a, b string) bool {
	return strings.EqualFold(strings.TrimSuffix(a, "."), strings.TrimSuffix(b, "."))
}

func requestHostname(host string) string {
	if name, _, err := net.SplitHostPort(host); err == nil {
		return name
	}
	return strings.TrimSuffix(strings.TrimPrefix(host, "["), "]")
}

// ClientKeys keys every per-client budget: a grant or login, then its subject at twice the share, else the
// address keys. Password logins share one subject, so each login needs its own share; ambiguous evidence has none.
func ClientKeys(r *http.Request, trusted []netip.Prefix) ([]string, bool) {
	if p, ok := PrincipalFromContext(r.Context()); ok {
		first := "login:" + p.session.id
		if p.grant != nil {
			first = "grant:" + p.grant.id
		}
		return []string{first, "principal:" + p.Subject}, true
	}
	client, ok := transport.ResolveClientAddress(r, trusted)
	if !ok {
		return nil, false
	}
	return transport.AddressKeys(client.Addr), true
}

func (s *Service) validRequestOrigin(r *http.Request, p Principal) bool {
	origin := r.Header.Get("Origin")
	if p.browserOrigin() != "" {
		return origin == p.browserOrigin() && browserGrantRoute(r.URL.Path)
	}
	if origin != "" && origin != s.origin {
		return false
	}
	if p.Bearer {
		return true
	}
	site := r.Header.Get("Sec-Fetch-Site")
	switch site {
	case "", "same-origin", "none":
	case "same-site":
		if origin != s.origin {
			return false
		}
	default:
		return false
	}
	measurement := isMeasurementRoute(r.URL.Path)
	read := r.Method == http.MethodGet || r.Method == http.MethodHead
	if p.session != nil && measurement && read && origin != s.origin && site != "same-origin" ||
		r.URL.Path == route.Ping && origin != s.origin {
		return false
	}
	if read || r.Method == http.MethodOptions {
		return true
	}
	return origin == s.origin &&
		(!measurement || p.session != nil && constantEqual(p.session.csrf, r.Header.Get("X-CSRF-Token")))
}

func (s *Service) checkCSRF(r *http.Request, field string) (reason, bool) {
	origin := r.Header.Get("Origin")
	if origin == "" {
		return reasonCSRFOriginMissing, false
	}
	if origin != s.origin {
		return reasonCSRFOriginMismatch, false
	}
	c := uniqueCookie(r, loginCookie)
	if c == nil {
		return reasonCSRFCookieMissing, false
	}
	v := r.FormValue(field)
	if v == "" {
		return reasonCSRFTokenMissing, false
	}
	if !constantEqual(c.Value, v) {
		return reasonCSRFTokenMismatch, false
	}
	return "", true
}

func constantEqual(a, b string) bool {
	return len(a) > 20 && len(a) == len(b) && subtle.ConstantTimeCompare([]byte(a), []byte(b)) == 1
}
