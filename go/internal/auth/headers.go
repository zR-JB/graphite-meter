package auth

// headers.go is the response-header policy: the CSP and hardening headers
// applied to authenticated and pre-auth responses, and the only CORS surface
// reachable without a principal — the headers-only measurement preflight.

import (
	"net/http"
	"strings"
)

func securityHeaders(h http.Header) {
	h.Set("Cache-Control", "no-store")
	h.Set("Referrer-Policy", "same-origin")
	h.Set("X-Content-Type-Options", "nosniff")
	h.Set("Permissions-Policy", "camera=(), microphone=(), geolocation=()")
	h.Set("Content-Security-Policy", authPageCSP(""))
}

// authPageCSP locks the no-JS login surface to its own inline stylesheet by
// hash; form-action widens only to the discovered authorization origin.
func authPageCSP(authorizationOrigin string) string {
	formAction := "'self'"
	if authorizationOrigin != "" {
		formAction += " " + authorizationOrigin
	}
	return "default-src 'none'; style-src 'sha256-" + authStyleHash + "'; form-action " + formAction + "; frame-ancestors 'none'; base-uri 'none'"
}

func (s *Service) loginSecurityHeaders(h http.Header) {
	securityHeaders(h)
	if s.oidc != nil {
		h.Set("Content-Security-Policy", authPageCSP(s.oidc.authorizationOrigin()))
	}
}

func authenticatedSecurityHeaders(h http.Header) {
	h.Set("Strict-Transport-Security", "max-age=31536000")
	h.Set("X-Frame-Options", "DENY")
	h.Set("Content-Security-Policy", "frame-ancestors 'none'")
	h.Set("Referrer-Policy", "same-origin")
	h.Set("X-Content-Type-Options", "nosniff")
	h.Set("Permissions-Policy", "camera=(), microphone=(), geolocation=()")
}

// corsPreflight answers the CORS preflight for measurement routes. It is the one
// unauthenticated path through Enforce, and writes headers only — never a body
// and never a principal-bearing response.
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

func allowedCORSMethod(path, method string) bool {
	switch path {
	case "/preflight", "/probe", "/download":
		return method == http.MethodGet
	case "/upload/session", "/upload":
		return method == http.MethodPost
	case "/upload/progress":
		return method == http.MethodGet || method == http.MethodDelete
	case "/ws/ping":
		return method == http.MethodGet
	}
	return false
}
