package server

import (
	"context"
	"net/http"
	"net/netip"
	"sync"
	"time"

	"github.com/zR-JB/graphite-meter/go/internal/auth"
	"github.com/zR-JB/graphite-meter/go/internal/endpoint"
)

type requestAdmission struct {
	mu               sync.Mutex
	active           int
	byClient         map[string]int
	sessionsByClient map[string]int
	globalMax        int
	clientMax        int
	sessionClientMax int
	requestLifetime  time.Duration
	sessionLifetime  time.Duration
	peak             int
	rejectedGlobal   uint64
	rejectedClient   uint64
}

func newRequestAdmission(globalMax, clientMax, sessionClientMax int, requestLifetime, sessionLifetime time.Duration) *requestAdmission {
	return &requestAdmission{byClient: make(map[string]int), sessionsByClient: make(map[string]int), globalMax: globalMax, clientMax: clientMax, sessionClientMax: sessionClientMax, requestLifetime: requestLifetime, sessionLifetime: sessionLifetime}
}

// isSessionRoute reports the routes a whole test rides, enumerated rather than
// prefix-matched so /wt/session, a plain mint POST, is not one of them.
func isSessionRoute(path string) bool {
	switch path {
	case routeWTDownload, routeWTUpload, routeWTPing:
		return true
	}
	return false
}

// lifetimeFor picks the bound a wrapped route lives under. A WebTransport
// session hosts a whole test and gets the session bound; every other wrapped
// route is one request or one reconnecting stream under the request bound.
func (a *requestAdmission) lifetimeFor(path string) time.Duration {
	if isSessionRoute(path) {
		return a.sessionLifetime
	}
	return a.requestLifetime
}

func (a *requestAdmission) acquire(key string, session bool) (release func(), status int) {
	a.mu.Lock()
	defer a.mu.Unlock()
	if a.byClient[key] >= a.clientMax || (session && a.sessionsByClient[key] >= a.sessionClientMax) {
		a.rejectedClient++
		return nil, http.StatusTooManyRequests
	}
	if a.active >= a.globalMax {
		a.rejectedGlobal++
		return nil, http.StatusServiceUnavailable
	}
	a.active++
	if a.active > a.peak {
		a.peak = a.active
	}
	a.byClient[key]++
	if session {
		a.sessionsByClient[key]++
	}
	return func() {
		a.mu.Lock()
		a.active--
		a.byClient[key]--
		if a.byClient[key] == 0 {
			delete(a.byClient, key)
		}
		if session {
			a.sessionsByClient[key]--
			if a.sessionsByClient[key] == 0 {
				delete(a.sessionsByClient, key)
			}
		}
		a.mu.Unlock()
	}, 0
}

type admissionStats struct {
	active, peak                   int
	rejectedGlobal, rejectedClient uint64
}

// load reports occupancy for the probe's saturation signal.
func (a *requestAdmission) load() (active, max int) {
	a.mu.Lock()
	defer a.mu.Unlock()
	return a.active, a.globalMax
}

func (a *requestAdmission) stats() admissionStats {
	a.mu.Lock()
	defer a.mu.Unlock()
	return admissionStats{a.active, a.peak, a.rejectedGlobal, a.rejectedClient}
}

// wrap accounts one in-flight measurement request. publicOrigin is the
// operator-configured origin, empty when authentication is off. A credentialed
// rejection echoes it instead of the request's own Origin.
func (a *requestAdmission) wrap(next http.Handler, trusted []netip.Prefix, publicOrigin string) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.Method == http.MethodOptions {
			next.ServeHTTP(w, r)
			return
		}
		release, status := a.acquire(endpoint.ClientKey(r, trusted), isSessionRoute(r.URL.Path))
		if status != 0 {
			setAdmissionHeaders(w, r, publicOrigin)
			w.Header().Set("Retry-After", "1")
			http.Error(w, http.StatusText(status), status)
			return
		}
		defer release()
		ctx, cancel := context.WithTimeout(r.Context(), a.lifetimeFor(r.URL.Path))
		defer cancel()
		// A socket deadline tears a long-lived channel down mid-stream, and on a
		// session route it would land on the stream carrying the closing capsule.
		// Those routes are bounded by their context alone.
		if deadline, ok := ctx.Deadline(); ok && r.URL.Path != routePing && !isSessionRoute(r.URL.Path) {
			defer setSocketDeadlines(w, deadline)()
		}
		next.ServeHTTP(w, r.WithContext(ctx))
	})
}

// setSocketDeadlines bounds a transfer that stops reading its context and
// returns the clear. Every protocol here carries them, HTTP/3 included.
func setSocketDeadlines(w http.ResponseWriter, deadline time.Time) func() {
	controller := http.NewResponseController(w)
	_ = controller.SetReadDeadline(deadline)
	_ = controller.SetWriteDeadline(deadline)
	return func() {
		_ = controller.SetReadDeadline(time.Time{})
		_ = controller.SetWriteDeadline(time.Time{})
	}
}

// setAdmissionHeaders writes the CORS headers for a rejected measurement
// request. A credentialed response echoes only the validated public origin:
// reflecting a request-supplied Origin alongside
// Access-Control-Allow-Credentials is a cross-origin read primitive.
func setAdmissionHeaders(w http.ResponseWriter, r *http.Request, publicOrigin string) {
	h := w.Header()
	if _, ok := auth.PrincipalFromContext(r.Context()); ok {
		if publicOrigin != "" && r.Header.Get("Origin") == publicOrigin {
			h.Set("Access-Control-Allow-Origin", publicOrigin)
			h.Set("Access-Control-Allow-Credentials", "true")
			h.Set("Timing-Allow-Origin", publicOrigin)
			h.Add("Vary", "Origin")
		}
		return
	}
	h.Set("Access-Control-Allow-Origin", "*")
	h.Set("Access-Control-Allow-Methods", "GET, POST, DELETE, OPTIONS")
	h.Set("Access-Control-Allow-Headers", "*")
	h.Set("Timing-Allow-Origin", "*")
}
