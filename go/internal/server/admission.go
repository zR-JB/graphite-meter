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
	mu              sync.Mutex
	active          int
	byClient        map[string]int
	globalMax       int
	clientMax       int
	requestLifetime time.Duration
	sessionLifetime time.Duration
	peak            int
	rejectedGlobal  uint64
	rejectedClient  uint64
}

func newRequestAdmission(globalMax, clientMax int, requestLifetime, sessionLifetime time.Duration) *requestAdmission {
	return &requestAdmission{byClient: make(map[string]int), globalMax: globalMax, clientMax: clientMax, requestLifetime: requestLifetime, sessionLifetime: sessionLifetime}
}

// lifetimeFor picks the bound a wrapped route lives under. A WebTransport
// session hosts a whole test and gets the session bound; every other wrapped
// route is one request or one reconnecting stream under the request bound. The
// session routes are enumerated rather than prefix-matched, so /wt/session, a
// plain mint POST, keeps the request bound if it is ever wrapped.
func (a *requestAdmission) lifetimeFor(path string) time.Duration {
	switch path {
	case routeWTDownload, routeWTUpload, routeWTPing:
		return a.sessionLifetime
	}
	return a.requestLifetime
}

func (a *requestAdmission) acquire(key string) (release func(), status int) {
	a.mu.Lock()
	defer a.mu.Unlock()
	if a.byClient[key] >= a.clientMax {
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
	return func() {
		a.mu.Lock()
		a.active--
		a.byClient[key]--
		if a.byClient[key] == 0 {
			delete(a.byClient, key)
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
		release, status := a.acquire(endpoint.ClientKey(r, trusted))
		if status != 0 {
			setAdmissionHeaders(w, r, publicOrigin)
			w.Header().Set("Retry-After", "1")
			http.Error(w, http.StatusText(status), status)
			return
		}
		defer release()
		ctx, cancel := context.WithTimeout(r.Context(), a.lifetimeFor(r.URL.Path))
		defer cancel()
		// A socket deadline tears the ping WebSocket down mid-stream. Its
		// context bounds that route alone.
		if deadline, ok := ctx.Deadline(); ok && r.URL.Path != routePing {
			defer setSocketDeadlines(w, deadline)()
		}
		next.ServeHTTP(w, r.WithContext(ctx))
	})
}

// setSocketDeadlines bounds a transfer that stops reading its context and
// returns the clear. HTTP/3 carries no deadlines and is served without.
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
