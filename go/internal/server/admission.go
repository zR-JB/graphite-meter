package server

import (
	"context"
	"net/http"
	"net/netip"
	"sync"
	"time"

	"github.com/zR-JB/graphite-meter/go/internal/auth"
	"github.com/zR-JB/graphite-meter/go/internal/transport"
)

type requestAdmission struct {
	mu             sync.Mutex
	active         int
	byClient       map[string]int
	globalMax      int
	clientMax      int
	maxLifetime    time.Duration
	peak           int
	rejectedGlobal uint64
	rejectedClient uint64
}

func newRequestAdmission(globalMax, clientMax int, maxLifetime time.Duration) *requestAdmission {
	return &requestAdmission{byClient: make(map[string]int), globalMax: globalMax, clientMax: clientMax, maxLifetime: maxLifetime}
}

func clientKey(r *http.Request, trusted []netip.Prefix) string {
	if p, ok := auth.PrincipalFromContext(r.Context()); ok {
		return "principal:" + p.Subject
	}
	addr := transport.ResolveClientAddress(r, trusted).Addr.Unmap()
	if !addr.IsValid() {
		return "unknown"
	}
	if addr.Is6() {
		return netip.PrefixFrom(addr, 64).Masked().String()
	}
	return addr.String()
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

func (a *requestAdmission) stats() admissionStats {
	a.mu.Lock()
	defer a.mu.Unlock()
	return admissionStats{a.active, a.peak, a.rejectedGlobal, a.rejectedClient}
}

func (a *requestAdmission) wrap(next http.Handler, trusted []netip.Prefix) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.Method == http.MethodOptions {
			next.ServeHTTP(w, r)
			return
		}
		release, status := a.acquire(clientKey(r, trusted))
		if status != 0 {
			setAdmissionHeaders(w, r)
			w.Header().Set("Retry-After", "1")
			http.Error(w, http.StatusText(status), status)
			return
		}
		defer release()
		ctx, cancel := context.WithTimeout(r.Context(), a.maxLifetime)
		defer cancel()
		if deadline, ok := ctx.Deadline(); ok && r.URL.Path != "/ws/ping" {
			controller := http.NewResponseController(w)
			_ = controller.SetReadDeadline(deadline)
			_ = controller.SetWriteDeadline(deadline)
			defer controller.SetReadDeadline(time.Time{})
			defer controller.SetWriteDeadline(time.Time{})
		}
		next.ServeHTTP(w, r.WithContext(ctx))
	})
}

func setAdmissionHeaders(w http.ResponseWriter, r *http.Request) {
	h := w.Header()
	if _, ok := auth.PrincipalFromContext(r.Context()); ok {
		if origin := r.Header.Get("Origin"); origin != "" {
			h.Set("Access-Control-Allow-Origin", origin)
			h.Set("Access-Control-Allow-Credentials", "true")
			h.Set("Timing-Allow-Origin", origin)
			h.Add("Vary", "Origin")
		}
		return
	}
	h.Set("Access-Control-Allow-Origin", "*")
	h.Set("Access-Control-Allow-Methods", "GET, POST, DELETE, OPTIONS")
	h.Set("Access-Control-Allow-Headers", "*")
	h.Set("Timing-Allow-Origin", "*")
}
