package auth

import "net/http"

const (
	measurementMethods = "GET, POST, DELETE, OPTIONS"
	authExposed        = "Graphite-Meter-Auth, Graphite-Meter-Auth-URL"
)

// MeasurementCORS exposes a measurement response to the one browser origin
// allowed to read it. Measurement handlers, admission refusals and
// authentication refusals all answer through it:
//   - public mode exposes every response to every origin, since it holds no session state;
//   - an approved browser grant's origin reads its own responses, without ambient cookies;
//   - the canonical UI origin reads with credentials;
//   - an unauthenticated secure origin may read a measurement refusal, to start its browser grant.
//
// A preflight also names the measurement methods and headers.
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
	case p.BrowserOrigin != "":
		bearerCORS(h, p.BrowserOrigin)
	case origin == s.origin:
		credentialedCORS(h, origin)
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

// credentialedCORS exposes a response, cookies included, to the canonical UI origin.
func credentialedCORS(h http.Header, origin string) {
	h.Set("Access-Control-Allow-Origin", origin)
	h.Set("Access-Control-Allow-Credentials", "true")
	h.Set("Access-Control-Expose-Headers", authExposed)
	h.Set("Timing-Allow-Origin", origin)
	h.Add("Vary", "Origin")
}

// bearerCORS exposes an explicitly authorized browser request without ambient cookies.
func bearerCORS(h http.Header, origin string) {
	h.Set("Access-Control-Allow-Origin", origin)
	h.Del("Access-Control-Allow-Credentials")
	h.Set("Access-Control-Expose-Headers", authExposed+", Graphite-Meter-Browser-Auth")
	h.Set("Timing-Allow-Origin", origin)
	h.Add("Vary", "Origin")
}
