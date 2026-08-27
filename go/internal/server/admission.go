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
	mu                                                 sync.Mutex
	active, activeSessions                             int
	byClient, sessionsByClient                         map[string]int
	globalMax, clientMax, sessionMax, sessionClientMax int
	requestLifetime, sessionLifetime                   time.Duration
	peak                                               int
	rejectedGlobal, rejectedClient                     uint64
	rejectedSessionBudget, rejectedSessionClient       uint64
}

func newRequestAdmission(globalMax, clientMax, sessionMax, sessionClientMax int, requestLifetime, sessionLifetime time.Duration) *requestAdmission {
	return &requestAdmission{byClient: make(map[string]int), sessionsByClient: make(map[string]int), globalMax: globalMax, clientMax: clientMax, sessionMax: sessionMax, sessionClientMax: sessionClientMax, requestLifetime: requestLifetime, sessionLifetime: sessionLifetime}
}

func isSessionRoute(path string) bool {
	return path == routeWTDownload || path == routeWTUpload
}

func isChannelRoute(path string) bool {
	return path == routePing || path == routeWTPing || isSessionRoute(path)
}

func (a *requestAdmission) lifetimeFor(path string) time.Duration {
	if isSessionRoute(path) {
		return a.sessionLifetime
	}
	return a.requestLifetime
}

func (a *requestAdmission) acquire(key, sessionKey string) (release func(), status int) {
	a.mu.Lock()
	defer a.mu.Unlock()
	session := sessionKey != ""
	counts, limit := a.byClient, a.clientMax
	if session {
		key, counts, limit = sessionKey, a.sessionsByClient, a.sessionClientMax
	}
	if counts[key] >= limit {
		if session {
			a.rejectedSessionClient++
		} else {
			a.rejectedClient++
		}
		return nil, http.StatusTooManyRequests
	}
	if a.active >= a.globalMax {
		a.rejectedGlobal++
		return nil, http.StatusServiceUnavailable
	}
	if session && a.activeSessions >= a.sessionMax {
		a.rejectedSessionBudget++
		return nil, http.StatusServiceUnavailable
	}
	a.active++
	a.peak = max(a.peak, a.active)
	if session {
		a.activeSessions++
	}
	counts[key]++
	return func() {
		a.mu.Lock()
		defer a.mu.Unlock()
		a.active--
		if session {
			a.activeSessions--
		}
		counts[key]--
		if counts[key] == 0 {
			delete(counts, key)
		}
	}, 0
}

type admissionStats struct {
	active, peak                   int
	rejectedGlobal, rejectedClient uint64
}

type requestAdmissionStats struct {
	admissionStats
	activeSessions, sessionMax int
	sessionClientMax           int
	rejectedSessionBudget      uint64
	rejectedSessionClient      uint64
}

func (a *requestAdmission) load() (active, max int) {
	a.mu.Lock()
	defer a.mu.Unlock()
	return a.active, a.globalMax
}

func (a *requestAdmission) stats() requestAdmissionStats {
	a.mu.Lock()
	defer a.mu.Unlock()
	return requestAdmissionStats{
		active:                a.active,
		peak:                  a.peak,
		rejectedGlobal:        a.rejectedGlobal,
		rejectedClient:        a.rejectedClient,
		activeSessions:        a.activeSessions,
		sessionMax:            a.sessionMax,
		sessionClientMax:      a.sessionClientMax,
		rejectedSessionBudget: a.rejectedSessionBudget,
		rejectedSessionClient: a.rejectedSessionClient,
	}
}

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
		if deadline, ok := ctx.Deadline(); ok && !isChannelRoute(r.URL.Path) {
			defer setSocketDeadlines(w, deadline)()
		}
		next.ServeHTTP(w, r.WithContext(ctx))
	})
}

func setSocketDeadlines(w http.ResponseWriter, deadline time.Time) func() {
	controller := http.NewResponseController(w)
	_ = controller.SetReadDeadline(deadline)
	_ = controller.SetWriteDeadline(deadline)
	return func() {
		_ = controller.SetReadDeadline(time.Time{})
		_ = controller.SetWriteDeadline(time.Time{})
	}
}

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
