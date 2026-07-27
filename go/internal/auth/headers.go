package auth

// headers.go is the response-header policy: the CSP and hardening headers
// applied to authenticated and pre-auth responses. The headers-only measurement
// preflight is the only CORS surface reachable without a principal.

import (
	"net/http"
	"strings"

	"github.com/zR-JB/graphite-meter/go/internal/static"
)

// appScriptHash pins the application's one inline pre-paint <script> in the
// authenticated CSP. It is derived from the embedded build at startup, so it
// tracks the served page exactly; it is "" when no real client is embedded
// (a Go-only build or test), and script-src is then omitted.
var appScriptHash = static.AppScriptCSPHash()

func securityHeaders(h http.Header) {
	h.Set("Cache-Control", "no-store")
	h.Set("Referrer-Policy", "same-origin")
	h.Set("X-Content-Type-Options", "nosniff")
	h.Set("Permissions-Policy", "camera=(), microphone=(), geolocation=()")
	h.Set("Content-Security-Policy", authPageCSP(""))
}

// authPageCSP is the login surface's policy: default-src 'none' admits nothing,
// and every source below is one the login page serves itself.
func authPageCSP(authorizationOrigin string) string {
	// form-action widens only to the discovered authorization origin, so the
	// OIDC sign-in form can post to the provider.
	formAction := "'self'"
	if authorizationOrigin != "" {
		formAction += " " + authorizationOrigin
	}
	return strings.Join([]string{
		"default-src 'none'",
		// The stylesheet and both scripts are inline and pinned by digest.
		// Scripting stays optional: the sign-in form posts natively.
		"style-src 'sha256-" + authStyleHash + "'",
		"script-src 'sha256-" + authThemeHash + "' 'sha256-" + authPendingHash + "'",
		// pending.js posts the same-origin sign-in forms with fetch, swapping
		// errors in place instead of navigating. It fetches nowhere else.
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

// appCSP is the application Content-Security-Policy. scriptHash pins the one
// inline pre-paint script; connectExtra is the cross-origin measurement targets
// from /preflight, so a script cannot reach any other host. img-src stays
// unpinned for the data: favicon. The login pages carry their own policy.
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

// hstsThisHostOnly omits includeSubDomains: a homelab runs many services under
// one base domain, and plain-HTTP or self-signed siblings break when forced to
// HTTPS. This host opts itself in and does not speak for its neighbours.
const hstsThisHostOnly = "max-age=31536000"

func (s *Service) authenticatedSecurityHeaders(h http.Header) {
	h.Set("Strict-Transport-Security", hstsThisHostOnly)
	h.Set("X-Frame-Options", "DENY")
	h.Set("Content-Security-Policy", appCSP(appScriptHash, s.connectSrc))
	h.Set("Referrer-Policy", "same-origin")
	h.Set("X-Content-Type-Options", "nosniff")
	h.Set("Permissions-Policy", "camera=(), microphone=(), geolocation=()")
}

// corsPreflight answers the CORS preflight for measurement routes. It is the one
// unauthenticated path through Enforce, and writes headers only: never a body,
// never a principal-bearing response.
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
	for _, raw := range strings.Split(r.Header.Get("Access-Control-Request-Headers"), ",") {
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

// allowedCORSMethod names the methods each measurement path may be preflighted
// for; an unlisted path is refused. The paths are pinned by api/routes.txt
// (routes_test.go).
func allowedCORSMethod(path, method string) bool {
	switch path {
	case "/preflight", "/probe", "/download":
		return method == http.MethodGet
	case "/upload/session", "/upload", "/wt/session":
		return method == http.MethodPost
	case "/upload/progress":
		return method == http.MethodGet || method == http.MethodDelete
	case "/ws/ping":
		return method == http.MethodGet
	// Session establishment is extended CONNECT; a browser never preflights it.
	case "/wt/download", "/wt/upload", "/wt/ping":
		return method == http.MethodConnect
	}
	return false
}
