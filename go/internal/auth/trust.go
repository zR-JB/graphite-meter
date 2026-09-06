package auth

import (
	"crypto/subtle"
	"net"
	"net/http"
	"net/netip"
	"net/url"
	"slices"
	"strings"

	"github.com/zR-JB/graphite-meter/go/internal/route"
)

type trust struct{ Secure, Canonical bool }

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
	return slices.ContainsFunc(ps, func(p netip.Prefix) bool { return p.Contains(a) })
}

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

func (s *Service) validRequestOrigin(r *http.Request, p Principal) bool {
	origin := r.Header.Get("Origin")
	if p.BrowserOrigin != "" {
		return origin == p.BrowserOrigin && browserGrantRoute(r.URL.Path)
	}
	if origin != "" && origin != s.public.String() {
		return false
	}
	if p.Bearer {
		return true
	}
	site := r.Header.Get("Sec-Fetch-Site")
	if site != "" && (site != "same-origin" && site != "same-site" && site != "none" || site == "same-site" && origin != s.public.String()) {
		return false
	}
	if p.session != nil && isMeasurementRoute(r.URL.Path) && (r.Method == http.MethodGet || r.Method == http.MethodHead) && origin != s.public.String() && site != "same-origin" {
		return false
	}
	if !s.wsPingOriginAllowed(r) {
		return false
	}
	if r.Method == http.MethodGet || r.Method == http.MethodHead || r.Method == http.MethodOptions {
		return true
	}
	return origin == s.public.String() && (!isMeasurementRoute(r.URL.Path) || p.session != nil && constantEqual(p.session.csrf, r.Header.Get("X-CSRF-Token")))
}

func (s *Service) wsPingOriginAllowed(r *http.Request) bool {
	return r.URL.Path != route.Ping || r.Header.Get("Origin") == s.public.String()
}

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
