package auth

import (
	"net/http"
	"strings"

	"github.com/zR-JB/graphite-meter/go/internal/route"
	"github.com/zR-JB/graphite-meter/go/internal/static"
)

var appScriptHash = static.AppScriptCSPHash()

func securityHeaders(h http.Header) {
	h.Set("Cache-Control", "no-store")
	hardeningHeaders(h)
	h.Set("Content-Security-Policy", authPageCSP(""))
}

func hardeningHeaders(h http.Header) {
	h.Set("Referrer-Policy", "same-origin")
	h.Set("X-Content-Type-Options", "nosniff")
	h.Set("Permissions-Policy", "camera=(), microphone=(), geolocation=()")
}

func authPageCSP(authorizationOrigin string) string {
	// form-action widens only to the discovered authorization origin, so the OIDC sign-in form can post to the provider.
	formAction := "'self'"
	if authorizationOrigin != "" {
		formAction += " " + authorizationOrigin
	}
	return strings.Join([]string{
		"default-src 'none'",
		"style-src 'sha256-" + authStyleHash + "'",
		"script-src 'sha256-" + authThemeHash + "' 'sha256-" + authPendingHash + "'",
		"connect-src 'self'",
		// data: covers the inlined favicon and admits no remote host.
		"img-src data:",
		"form-action " + formAction,
		"frame-ancestors 'none'",
		"base-uri 'none'",
	}, "; ")
}

func (s *Service) loginSecurityHeaders(h http.Header) {
	securityHeaders(h)
	if s.oidc != nil {
		h.Set("Content-Security-Policy", authPageCSP(s.oidc.authorizationOrigin()))
	}
}

func appCSP(scriptHash, connectExtra string) string {
	csp := "frame-ancestors 'none'; base-uri 'none'; object-src 'none'; form-action 'self'; connect-src 'self'"
	if connectExtra != "" {
		csp += " " + connectExtra
	}
	if scriptHash != "" {
		csp += "; script-src 'self' 'sha256-" + scriptHash + "'"
	}
	return csp
}

const hstsThisHostOnly = "max-age=31536000"

func (s *Service) authenticatedSecurityHeaders(h http.Header) {
	h.Set("Strict-Transport-Security", hstsThisHostOnly)
	h.Set("X-Frame-Options", "DENY")
	h.Set("Content-Security-Policy", appCSP(appScriptHash, s.connectSrc))
	hardeningHeaders(h)
}

func (s *Service) corsPreflight(w http.ResponseWriter, r *http.Request, secure bool) {
	if !secure || r.Header.Get("Origin") != s.public.String() {
		forbidden(w)
		return
	}
	method := r.Header.Get("Access-Control-Request-Method")
	if !allowedCORSMethod(r.URL.Path, method) {
		forbidden(w)
		return
	}
	for raw := range strings.SplitSeq(r.Header.Get("Access-Control-Request-Headers"), ",") {
		h := strings.ToLower(strings.TrimSpace(raw))
		if h != "" && h != "authorization" && h != "content-type" && h != "x-csrf-token" {
			forbidden(w)
			return
		}
	}
	h := w.Header()
	h.Set("Access-Control-Allow-Origin", s.public.String())
	h.Set("Access-Control-Allow-Credentials", "true")
	h.Set("Access-Control-Allow-Methods", "GET, POST, DELETE, OPTIONS")
	h.Set("Access-Control-Allow-Headers", "Authorization, Content-Type, X-CSRF-Token")
	h.Set("Access-Control-Expose-Headers", "Graphite-Meter-Auth, Graphite-Meter-Auth-URL")
	h.Set("Timing-Allow-Origin", s.public.String())
	h.Add("Vary", "Origin")
	w.WriteHeader(http.StatusNoContent)
}

func allowedCORSMethod(path, method string) bool {
	spec, ok := route.Lookup(path)
	return ok && spec.AllowsCORSMethod(method)
}
