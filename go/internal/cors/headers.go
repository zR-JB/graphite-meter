// Package cors writes measurement CORS headers after the caller has checked access policy.
package cors

import "net/http"

// Response exposes measurements to an approved deployment origin. An empty origin is the public, unauthenticated mode.
func Response(h http.Header, origin string) {
	if origin == "" {
		h.Set("Access-Control-Allow-Origin", "*")
		h.Set("Timing-Allow-Origin", "*")
		return
	}
	h.Set("Access-Control-Allow-Origin", origin)
	h.Set("Access-Control-Allow-Credentials", "true")
	h.Set("Access-Control-Expose-Headers", "Graphite-Meter-Auth, Graphite-Meter-Auth-URL")
	h.Set("Timing-Allow-Origin", origin)
	h.Add("Vary", "Origin")
}

// Bearer exposes an explicitly authorized browser request without ambient cookies.
func Bearer(h http.Header, origin string) {
	h.Set("Access-Control-Allow-Origin", origin)
	h.Del("Access-Control-Allow-Credentials")
	h.Set("Access-Control-Expose-Headers", "Graphite-Meter-Auth, Graphite-Meter-Auth-URL, Graphite-Meter-Browser-Auth")
	h.Set("Timing-Allow-Origin", origin)
	h.Add("Vary", "Origin")
}

// Measurement includes the shared preflight declarations; callers still validate the requested route, method and headers.
func Measurement(h http.Header, origin string) {
	Response(h, origin)
	h.Set("Access-Control-Allow-Methods", "GET, POST, DELETE, OPTIONS")
	if origin == "" {
		h.Set("Access-Control-Allow-Headers", "*")
	} else {
		h.Set("Access-Control-Allow-Headers", "Authorization, Content-Type, X-CSRF-Token")
	}
}
