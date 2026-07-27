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
	activeSessions   int
	byClient         map[string]int
	sessionsByClient map[string]int
	globalMax        int
	clientMax        int
	sessionMax       int
	sessionClientMax int
	requestLifetime  time.Duration
	sessionLifetime  time.Duration
	peak             int
	rejectedGlobal   uint64
	rejectedClient   uint64
	// rejectedSessionBudget is kept apart from rejectedGlobal: both answer 503,
	// but a full pool and a full session budget are raised with different knobs.
	rejectedSessionBudget uint64
	// rejectedSessionClient is the per-login session bound, which at its default
	// binds before either of the others. Folded into rejectedClient it read as
	// the handler bound, and raising that one changes nothing.
	rejectedSessionClient uint64
}

func newRequestAdmission(globalMax, clientMax, sessionMax, sessionClientMax int, requestLifetime, sessionLifetime time.Duration) *requestAdmission {
	return &requestAdmission{byClient: make(map[string]int), sessionsByClient: make(map[string]int), globalMax: globalMax, clientMax: clientMax, sessionMax: sessionMax, sessionClientMax: sessionClientMax, requestLifetime: requestLifetime, sessionLifetime: sessionLifetime}
}

// isSessionRoute reports the routes a whole test rides: the transfer sessions,
// which hold lanes and their drain buffers for the length of a stage. The
// datagram ping bus is not one of them. It carries the same message protocol as
// the WebSocket bus, costs the same nothing to hold, and reconnects the same
// way, so it lives under the same bound rather than being treated as a test
// because of the mechanism underneath it. Enumerated rather than prefix-matched
// so /wt/session, a plain mint POST, is not one either.
func isSessionRoute(path string) bool {
	switch path {
	case routeWTDownload, routeWTUpload:
		return true
	}
	return false
}

// isChannelRoute names the routes that hold a channel open rather than serving a
// request, so a socket deadline would tear one down mid-stream.
func isChannelRoute(path string) bool {
	switch path {
	case routePing, routeWTPing:
		return true
	}
	return isSessionRoute(path)
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

// acquire admits one measurement. sessionKey is empty on a request-shaped route;
// on a session route it is the separate bucket that route's budget is kept in.
func (a *requestAdmission) acquire(key, sessionKey string) (release func(), status int) {
	a.mu.Lock()
	defer a.mu.Unlock()
	session := sessionKey != ""
	clientFull := a.byClient[key] >= a.clientMax
	if session {
		clientFull = a.sessionsByClient[sessionKey] >= a.sessionClientMax
	}
	if clientFull {
		if session {
			a.rejectedSessionClient++
		} else {
			a.rejectedClient++
		}
		return nil, http.StatusTooManyRequests
	}
	// A session takes a slot from the global pool AND from the session share of
	// it, because it holds that slot for hours rather than for one request.
	// The share runs one way: it caps sessions and reserves nothing, so a pool
	// filled by request-shaped routes refuses every session while
	// activeSessions is zero. Two 503s with two remedies, so two counters. The
	// pool is tested first, since a session needs a slot in it either way.
	if a.active >= a.globalMax {
		a.rejectedGlobal++
		return nil, http.StatusServiceUnavailable
	}
	if session && a.activeSessions >= a.sessionMax {
		a.rejectedSessionBudget++
		return nil, http.StatusServiceUnavailable
	}
	a.active++
	if a.active > a.peak {
		a.peak = a.active
	}
	if session {
		a.activeSessions++
		a.sessionsByClient[sessionKey]++
	} else {
		a.byClient[key]++
	}
	return func() {
		a.mu.Lock()
		a.active--
		if session {
			a.activeSessions--
			a.sessionsByClient[sessionKey]--
			if a.sessionsByClient[sessionKey] == 0 {
				delete(a.sessionsByClient, sessionKey)
			}
		} else {
			a.byClient[key]--
			if a.byClient[key] == 0 {
				delete(a.byClient, key)
			}
		}
		a.mu.Unlock()
	}, 0
}

type admissionStats struct {
	active, peak                   int
	rejectedGlobal, rejectedClient uint64
}

// requestAdmissionStats is admissionStats plus what only the measurement pool
// has: the session budget's own occupancy and its own refusals. The two 503s
// are raised with different knobs -- GM_MAX_ACTIVE_MEASUREMENTS against a full
// pool, GM_MAX_ACTIVE_SESSIONS against a full session budget -- so one counter
// for both told an operator a limit had been hit but not which one to raise.
// Occupancy is the same problem read forward: sessions hold their slots for the
// session bound, so a full session budget is the state that lasts, and it was
// the one number nothing anywhere reported.
type requestAdmissionStats struct {
	admissionStats
	activeSessions, sessionMax int
	sessionClientMax           int
	rejectedSessionBudget      uint64
	rejectedSessionClient      uint64
}

// load reports occupancy for the probe's saturation signal.
func (a *requestAdmission) load() (active, max int) {
	a.mu.Lock()
	defer a.mu.Unlock()
	return a.active, a.globalMax
}

func (a *requestAdmission) stats() requestAdmissionStats {
	a.mu.Lock()
	defer a.mu.Unlock()
	return requestAdmissionStats{
		admissionStats:        admissionStats{a.active, a.peak, a.rejectedGlobal, a.rejectedClient},
		activeSessions:        a.activeSessions,
		sessionMax:            a.sessionMax,
		sessionClientMax:      a.sessionClientMax,
		rejectedSessionBudget: a.rejectedSessionBudget,
		rejectedSessionClient: a.rejectedSessionClient,
	}
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
		sessionKey := ""
		if isSessionRoute(r.URL.Path) {
			sessionKey = endpoint.SessionKey(r, trusted)
		}
		release, status := a.acquire(endpoint.ClientKey(r, trusted), sessionKey)
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
		if deadline, ok := ctx.Deadline(); ok && !isChannelRoute(r.URL.Path) {
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
