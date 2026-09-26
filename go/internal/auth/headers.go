package auth

import (
	"net/http"
	"slices"
	"strings"

	"github.com/zR-JB/graphite-meter/go/internal/route"
	"github.com/zR-JB/graphite-meter/go/internal/static"
)

const (
	measurementMethods = "GET, POST, DELETE, OPTIONS"
	authExposed        = "Graphite-Meter-Auth, Graphite-Meter-Auth-URL"
	hstsThisHostOnly   = "max-age=31536000"
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

func (s *Service) authenticatedSecurityHeaders(h http.Header) {
	h.Set("Strict-Transport-Security", hstsThisHostOnly)
	h.Set("X-Frame-Options", "DENY")
	h.Set("Content-Security-Policy", appCSP(appScriptHash, s.connectSrc))
	hardeningHeaders(h)
}

func (s *Service) corsPreflight(w http.ResponseWriter, r *http.Request, t trust) {
	origin := r.Header.Get("Origin")
	method := r.Header.Get("Access-Control-Request-Method")
	clientOrigin, browser := secureBrowserOrigin(origin)
	if r.URL.Path == "/auth/browser/token" {
		if _, ok := requestedHeaders(r, "content-type"); !ok || !t.Secure || !t.Canonical || !browser ||
			method != http.MethodPost {
			forbidden(w)
			return
		}
		allowBearerPreflight(w, clientOrigin, http.MethodPost, "Content-Type")
		return
	}
	if t.Secure && browser && clientOrigin != s.origin && browserGrantRoute(r.URL.Path) {
		headers, ok := requestedHeaders(r, "authorization", "content-type")
		if !ok || !slices.Contains(headers, "authorization") || !allowedCORSMethod(r.URL.Path, method) {
			forbidden(w)
			return
		}
		allowBearerPreflight(w, clientOrigin, measurementMethods, "Authorization, Content-Type")
		return
	}
	_, ok := requestedHeaders(r, "authorization", "content-type", "x-csrf-token")
	if !t.Secure || origin != s.origin || !allowedCORSMethod(r.URL.Path, method) || !ok {
		forbidden(w)
		return
	}
	s.MeasurementCORS(w.Header(), r)
	w.WriteHeader(http.StatusNoContent)
}

func allowBearerPreflight(w http.ResponseWriter, origin, methods, headers string) {
	bearerCORS(w.Header(), origin)
	w.Header().Set("Access-Control-Allow-Methods", methods)
	w.Header().Set("Access-Control-Allow-Headers", headers)
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

// MeasurementCORS exposes a measurement response or refusal to the origin allowed to read it:
// any origin in public mode, a grant's own origin, the UI origin with credentials, or an
// unauthenticated secure origin reading the refusal that starts its grant.
func (s *Service) MeasurementCORS(h http.Header, r *http.Request) {
	if !s.Enabled() {
		h.Set("Access-Control-Allow-Origin", "*")
		h.Set("Timing-Allow-Origin", "*")
		if r.Method == http.MethodOptions {
			h.Set("Access-Control-Allow-Methods", measurementMethods)
			h.Set("Access-Control-Allow-Headers", "*")
		}
		return
	}
	p, authenticated := PrincipalFromContext(r.Context())
	origin := r.Header.Get("Origin")
	switch {
	case p.browserOrigin() != "":
		bearerCORS(h, p.browserOrigin())
	case origin == s.origin:
		h.Set("Access-Control-Allow-Origin", origin)
		h.Set("Access-Control-Allow-Credentials", "true")
		h.Set("Access-Control-Expose-Headers", authExposed)
		h.Set("Timing-Allow-Origin", origin)
		h.Add("Vary", "Origin")
		if r.Method == http.MethodOptions {
			h.Set("Access-Control-Allow-Methods", measurementMethods)
			h.Set("Access-Control-Allow-Headers", "Authorization, Content-Type, X-CSRF-Token")
		}
	case !authenticated && isMeasurementRoute(r.URL.Path):
		if canonical, valid := secureBrowserOrigin(origin); valid {
			bearerCORS(h, canonical)
		}
	}
}

// bearerCORS exposes a grant-authorized response without ambient cookies.
func bearerCORS(h http.Header, origin string) {
	h.Set("Access-Control-Allow-Origin", origin)
	h.Del("Access-Control-Allow-Credentials")
	h.Set("Access-Control-Expose-Headers", authExposed+", Graphite-Meter-Browser-Auth")
	h.Set("Timing-Allow-Origin", origin)
	h.Add("Vary", "Origin")
}
