package auth

// trust.go holds what the service believes about a request and why: whether it
// arrived over TLS on the canonical host, which address it is budgeted
// against, and whether its origin and CSRF evidence permit the action.

import (
	"crypto/subtle"
	"net"
	"net/http"
	"net/netip"
	"net/url"
	"strings"
)

// trust is what the service is willing to believe about a request: that it
// arrived over TLS (Secure) and that it addressed the canonical public host
// (Canonical). Named fields rather than a bare (bool, bool) so the two cannot
// be swapped at the boundary that decides whether authentication runs at all.
type trust struct{ Secure, Canonical bool }

// requestTrust evaluates a request against the trust boundary. Direct TLS is
// believed on its own; a cleartext request is believed only from a configured
// trusted proxy, and only when a single, non-list X-Forwarded-Proto and
// X-Forwarded-Host say https and the canonical host. Anything else is
// untrusted, which is also what an untrusted peer forging those headers gets.
func (s *Service) requestTrust(r *http.Request) trust {
	if r.TLS != nil {
		return trust{Secure: true, Canonical: equalHost(r.Host, s.public.Host)}
	}
	peer, err := splitRemote(r.RemoteAddr)
	if err != nil || !prefixContains(s.trusted, peer) {
		return trust{}
	}
	proto := singleHeader(r.Header, "X-Forwarded-Proto")
	host := singleHeader(r.Header, "X-Forwarded-Host")
	forwarded := proto == "https" && equalHost(host, s.public.Host)
	return trust{Secure: forwarded, Canonical: forwarded}
}

// singleHeader returns a header value only when it appears exactly once and
// carries no comma-joined list, so a spoofed second value cannot be smuggled
// past a trusted proxy's own header.
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
	u, err := url.Parse("//" + host)
	if err != nil {
		return ""
	}
	return u.Hostname()
}

func splitRemote(raw string) (netip.Addr, error) {
	host, _, err := net.SplitHostPort(raw)
	if err != nil {
		host = raw
	}
	return netip.ParseAddr(strings.Trim(host, "[]"))
}

func prefixContains(ps []netip.Prefix, a netip.Addr) bool {
	if a.Is4In6() {
		a = a.Unmap()
	}
	for _, p := range ps {
		if p.Contains(a) {
			return true
		}
	}
	return false
}

// authClientAddress resolves the address an attempt budget is charged to. A
// direct peer is charged as itself; behind a trusted proxy only a single
// X-Real-IP is accepted, and the ambiguous Forwarded / X-Forwarded-For pair
// fails closed.
func (s *Service) authClientAddress(r *http.Request) (netip.Addr, bool) {
	peer, err := splitRemote(r.RemoteAddr)
	if err != nil {
		return netip.Addr{}, false
	}
	peer = peer.Unmap()
	if !prefixContains(s.trusted, peer) {
		return peer, true
	}
	if r.Header.Get("Forwarded") != "" || r.Header.Get("X-Forwarded-For") != "" {
		return netip.Addr{}, false
	}
	raw := singleHeader(r.Header, "X-Real-IP")
	addr, err := netip.ParseAddr(raw)
	if err != nil {
		return netip.Addr{}, false
	}
	return addr.Unmap(), true
}

// validRequestOrigin enforces the origin, Sec-Fetch-Site, and double-submit
// CSRF policy for an authenticated request.
func (s *Service) validRequestOrigin(r *http.Request, p Principal) bool {
	if p.Bearer {
		return r.Header.Get("Origin") == "" || r.Header.Get("Origin") == s.public.String()
	}
	if origin := r.Header.Get("Origin"); origin != "" && origin != s.public.String() {
		return false
	}
	if site := r.Header.Get("Sec-Fetch-Site"); site != "" {
		if site == "same-site" && r.Header.Get("Origin") != s.public.String() {
			return false
		}
		if site != "same-origin" && site != "same-site" && site != "none" {
			return false
		}
	}
	if p.session != nil && isMeasurementRoute(r.URL.Path) && (r.Method == http.MethodGet || r.Method == http.MethodHead) {
		if r.Header.Get("Origin") != s.public.String() && r.Header.Get("Sec-Fetch-Site") != "same-origin" {
			return false
		}
	}
	if r.URL.Path == "/ws/ping" && r.Header.Get("Origin") != s.public.String() {
		return false
	}
	if r.Method != http.MethodGet && r.Method != http.MethodHead && r.Method != http.MethodOptions {
		if r.Header.Get("Origin") != s.public.String() {
			return false
		}
		if isMeasurementRoute(r.URL.Path) && (p.session == nil || !constantEqual(p.session.csrf, r.Header.Get("X-CSRF-Token"))) {
			return false
		}
	}
	return true
}

// checkCSRF validates the pre-session login form's double-submit token. It
// reports the classified reason and whether the request may proceed; callers
// must branch on ok, never on the reason.
func (s *Service) checkCSRF(r *http.Request, field string) (reason, bool) {
	origin := r.Header.Get("Origin")
	if origin == "" {
		return reasonCSRFOriginMissing, false
	}
	if origin != s.public.String() {
		return reasonCSRFOriginMismatch, false
	}
	c, err := r.Cookie(loginCookie)
	if err != nil {
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
