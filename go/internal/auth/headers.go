package auth

import (
	"net/http"
	"slices"
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
	// The OIDC sign-in form posts to the discovered authorization origin.
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
	origin := r.Header.Get("Origin")
	method := r.Header.Get("Access-Control-Request-Method")
	clientOrigin, valid := secureBrowserOrigin(origin)
	if secure && valid && clientOrigin != s.origin && browserGrantRoute(r.URL.Path) {
		headers, allowed := requestedHeaders(r, "authorization", "content-type")
		if !allowed || !slices.Contains(headers, "authorization") || !allowedCORSMethod(r.URL.Path, method) {
			forbidden(w)
			return
		}
		bearerCORS(w.Header(), clientOrigin)
		w.Header().Set("Access-Control-Allow-Methods", measurementMethods)
		w.Header().Set("Access-Control-Allow-Headers", "Authorization, Content-Type")
		w.WriteHeader(http.StatusNoContent)
		return
	}
	_, allowed := requestedHeaders(r, "authorization", "content-type", "x-csrf-token")
	if !secure || origin != s.origin || !allowedCORSMethod(r.URL.Path, method) || !allowed {
		forbidden(w)
		return
	}
	s.MeasurementCORS(w.Header(), r)
	w.WriteHeader(http.StatusNoContent)
}

func requestedHeaders(r *http.Request, allowed ...string) ([]string, bool) {
	var names []string
	for raw := range strings.SplitSeq(r.Header.Get("Access-Control-Request-Headers"), ",") {
		name := strings.ToLower(strings.TrimSpace(raw))
		if name == "" {
			continue
		}
		if !slices.Contains(allowed, name) {
			return nil, false
		}
		names = append(names, name)
	}
	return names, true
}

func allowedCORSMethod(path, method string) bool {
	spec, ok := route.Lookup(path)
	return ok && spec.AllowsCORSMethod(method)
}
