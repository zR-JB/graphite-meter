package server

import (
	"context"
	"net/http"
	"net/http/httptest"
	"net/netip"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/coder/websocket"
	"github.com/zR-JB/graphite-meter/go/internal/endpoint"
	"github.com/zR-JB/graphite-meter/go/internal/transport"
)

func TestRequestAdmissionPerClientAndRelease(t *testing.T) {
	a := newRequestAdmission(3, 2, 3, 4, time.Minute, time.Hour)
	entered := make(chan struct{}, 2)
	release := make(chan struct{})
	h := a.wrap(http.HandlerFunc(func(http.ResponseWriter, *http.Request) {
		entered <- struct{}{}
		<-release
	}), nil, "")

	var wg sync.WaitGroup
	for i := 0; i < 2; i++ {
		wg.Add(1)
		go func() {
			defer wg.Done()
			r := httptest.NewRequest(http.MethodGet, "/download", nil)
			r.RemoteAddr = "192.0.2.10:1234"
			h.ServeHTTP(httptest.NewRecorder(), r)
		}()
	}
	<-entered
	<-entered
	r := httptest.NewRequest(http.MethodGet, "/download", nil)
	r.RemoteAddr = "192.0.2.10:5678"
	w := httptest.NewRecorder()
	h.ServeHTTP(w, r)
	if w.Code != http.StatusTooManyRequests || w.Header().Get("Retry-After") != "1" || w.Header().Get("Access-Control-Allow-Origin") != "*" {
		t.Fatalf("rejection = %d headers %v, want 429 with Retry-After 1 and a wildcard origin", w.Code, w.Header())
	}
	close(release)
	wg.Wait()
	w = httptest.NewRecorder()
	h.ServeHTTP(w, httptest.NewRequest(http.MethodGet, "/download", nil))
	if w.Code != http.StatusOK {
		t.Fatalf("request after release = %d, want %d", w.Code, http.StatusOK)
	}
}

func TestRequestAdmissionGlobalLimit(t *testing.T) {
	a := newRequestAdmission(1, 1, 1, 4, time.Minute, time.Hour)
	release, status := a.acquire("192.0.2.1", "")
	if status != 0 {
		t.Fatal("first request rejected")
	}
	defer release()
	if _, status := a.acquire("192.0.2.2", ""); status != http.StatusServiceUnavailable {
		t.Fatalf("global rejection = %d, want %d", status, http.StatusServiceUnavailable)
	}
	stats := a.stats()
	if stats.active != 1 || stats.peak != 1 || stats.rejectedGlobal != 1 {
		t.Fatalf("stats = %+v, want 1 active, 1 peak, 1 global rejection", stats)
	}
}

func TestClientKeyGroupsIPv6ByPrefix(t *testing.T) {
	a := httptest.NewRequest(http.MethodGet, "/", nil)
	b := httptest.NewRequest(http.MethodGet, "/", nil)
	a.RemoteAddr = "[2001:db8:1::1]:1"
	b.RemoteAddr = "[2001:db8:1::ffff]:2"
	if endpoint.ClientKey(a, nil) != endpoint.ClientKey(b, nil) {
		t.Fatalf("same /64 produced %q and %q", endpoint.ClientKey(a, nil), endpoint.ClientKey(b, nil))
	}
}

func TestClientKeyUsesTrustedForwardedAddress(t *testing.T) {
	r := httptest.NewRequest(http.MethodGet, "/", nil)
	r.RemoteAddr = "10.0.0.2:1234"
	r.Header.Set("X-Forwarded-For", "198.51.100.9")
	trusted := []netip.Prefix{netip.MustParsePrefix("10.0.0.0/8")}
	if got := endpoint.ClientKey(r, trusted); got != "198.51.100.9" {
		t.Fatalf("client key = %q, want %q", got, "198.51.100.9")
	}
}

func TestRequestAdmissionLifetime(t *testing.T) {
	a := newRequestAdmission(1, 1, 1, 4, 10*time.Millisecond, time.Hour)
	done := make(chan struct{})
	h := a.wrap(http.HandlerFunc(func(_ http.ResponseWriter, r *http.Request) {
		<-r.Context().Done()
		close(done)
	}), nil, "")
	h.ServeHTTP(httptest.NewRecorder(), httptest.NewRequestWithContext(context.Background(), http.MethodGet, "/", nil))
	select {
	case <-done:
	default:
		t.Fatal("handler did not observe lifetime deadline")
	}
}

func TestRequestAdmissionSessionRouteUsesSessionLifetime(t *testing.T) {
	a := newRequestAdmission(1, 1, 1, 4, time.Minute, 10*time.Millisecond)
	done := make(chan struct{})
	h := a.wrap(http.HandlerFunc(func(_ http.ResponseWriter, r *http.Request) {
		<-r.Context().Done()
		close(done)
	}), nil, "")
	h.ServeHTTP(httptest.NewRecorder(), httptest.NewRequest(http.MethodGet, "/wt/download", nil))
	select {
	case <-done:
	default:
		t.Fatal("session route did not observe the session deadline")
	}
}

// Session routes carry their own per-client budget, since one holds a slot for
// a whole test rather than a request. The bound is configurable: a deployment
// behind CGNAT collapses many users onto one address.
func TestRequestAdmissionBoundsSessionsPerClient(t *testing.T) {
	a := newRequestAdmission(100, 100, 100, 2, time.Minute, time.Hour)
	first, status := a.acquire("client", "session")
	if status != 0 {
		t.Fatalf("first session rejected with %d", status)
	}
	second, status := a.acquire("client", "session")
	if status != 0 {
		t.Fatalf("second session rejected with %d", status)
	}
	if _, status := a.acquire("client", "session"); status != http.StatusTooManyRequests {
		t.Fatalf("third session = %d, want %d", status, http.StatusTooManyRequests)
	}
	// The budget is the session routes' own: ordinary requests still pass.
	if _, status := a.acquire("client", ""); status != 0 {
		t.Fatalf("request rejected while sessions were full: %d", status)
	}
	first()
	if release, status := a.acquire("client", "session"); status != 0 {
		t.Fatalf("session rejected after release: %d", status)
	} else {
		release()
	}
	second()
}

// Session routes carry their own share of the global pool as well as their own
// per-client bucket. A session's slot is held for the session bound, so without
// the global share a few clients' sessions occupy the pool for hours and every
// request-shaped route is refused behind them.
func TestRequestAdmissionBoundsSessionsGlobally(t *testing.T) {
	// Room for ten measurements and ten sessions per client, but only two
	// sessions overall: nothing per-client is what refuses the third.
	a := newRequestAdmission(10, 10, 2, 10, time.Minute, time.Hour)
	first, status := a.acquire("client-a", "login-a")
	if status != 0 {
		t.Fatalf("first session rejected with %d", status)
	}
	second, status := a.acquire("client-b", "login-b")
	if status != 0 {
		t.Fatalf("second session rejected with %d", status)
	}
	if _, status := a.acquire("client-c", "login-c"); status != http.StatusServiceUnavailable {
		t.Fatalf("session past the session budget = %d, want %d", status, http.StatusServiceUnavailable)
	}
	// The property the single global counter lost: the pool still admits the
	// request-shaped routes while every session slot is taken.
	for _, key := range []string{"client-c", "client-d"} {
		release, status := a.acquire(key, "")
		if status != 0 {
			t.Fatalf("request from %s rejected while the session budget was full: %d", key, status)
		}
		release()
	}
	first()
	release, status := a.acquire("client-c", "login-c")
	if status != 0 {
		t.Fatalf("session rejected after a session slot was released: %d", status)
	}
	release()
	second()
	if stats := a.stats(); stats.active != 0 || stats.rejectedGlobal != 1 {
		t.Fatalf("stats = %+v, want no active measurements and 1 global rejection", stats)
	}
}

// The session budget caps what sessions may occupy and reserves nothing for
// them, which is what the documentation now claims. The ping buses are
// deliberately request-shaped, so enough of them fill the pool and every session
// is refused with the session budget untouched.
func TestSessionBudgetIsACeilingNotAReservation(t *testing.T) {
	sessionKeyFor := func(path, login string) string {
		if isSessionRoute(path) {
			return login
		}
		return ""
	}
	// Room for four measurements and four sessions, two per client either way.
	a := newRequestAdmission(4, 2, 4, 2, time.Minute, time.Hour)
	for i, key := range []string{"client-a", "client-a", "client-b", "client-b"} {
		release, status := a.acquire(key, sessionKeyFor(routeWTPing, "login-"+key))
		if status != 0 {
			t.Fatalf("ping bus %d from %s rejected with %d", i, key, status)
		}
		defer release()
	}
	if _, status := a.acquire("client-c", sessionKeyFor(routeWTDownload, "login-c")); status != http.StatusServiceUnavailable {
		t.Fatalf("session against a pool held by request-shaped routes = %d, want %d", status, http.StatusServiceUnavailable)
	}
	a.mu.Lock()
	active := a.activeSessions
	a.mu.Unlock()
	if active != 0 {
		t.Fatalf("activeSessions = %d, want 0: the refusal came from the pool, not from the session budget", active)
	}
}

// The session budget is per login, not per person. A budget keyed by subject is
// shared by every tab, browser and device, so sessions held on a phone decide
// whether a desktop can run a test at all.
func TestSessionBudgetIsPerLogin(t *testing.T) {
	// Equal request and session limits are valid. Filling one login's session
	// bucket must consume neither another login's bucket nor the subject-keyed
	// request bucket.
	a := newRequestAdmission(100, 1, 100, 1, time.Minute, time.Hour)
	first, status := a.acquire("client", "login:phone")
	if status != 0 {
		t.Fatalf("first login rejected with %d", status)
	}
	defer first()
	second, status := a.acquire("client", "login:desktop")
	if status != 0 {
		t.Fatalf("second login rejected with %d while the first held its slot", status)
	}
	defer second()
	if _, status := a.acquire("client", "login:phone"); status != http.StatusTooManyRequests {
		t.Fatalf("same login past its budget = %d, want %d", status, http.StatusTooManyRequests)
	}
	request, status := a.acquire("client", "")
	if status != 0 {
		t.Fatalf("ordinary request rejected while session buckets were full: %d", status)
	}
	request()
}

// A principal with no login falls back to the address, so public mode keeps the
// per-client budget it had before logins existed.
func TestSessionKeyFallsBackToTheClientKey(t *testing.T) {
	r := httptest.NewRequest(http.MethodGet, "/wt/download", nil)
	r.RemoteAddr = "192.0.2.7:1234"
	if got, want := endpoint.SessionKey(r, nil), endpoint.ClientKey(r, nil); got != want {
		t.Fatalf("session key = %q, want the client key %q", got, want)
	}
}

// The two ping buses are one thing under two mechanisms: neither holds a test,
// so neither takes the session bound or the session budget.
func TestPingBusesShareTheRequestBound(t *testing.T) {
	a := newRequestAdmission(100, 100, 100, 1, time.Minute, time.Hour)
	for _, path := range []string{routePing, routeWTPing} {
		if got := a.lifetimeFor(path); got != a.requestLifetime {
			t.Errorf("%s lifetime = %v, want the request bound %v", path, got, a.requestLifetime)
		}
		if isSessionRoute(path) {
			t.Errorf("%s counts against the session budget", path)
		}
	}
	for _, path := range []string{routeWTDownload, routeWTUpload} {
		if got := a.lifetimeFor(path); got != a.sessionLifetime {
			t.Errorf("%s lifetime = %v, want the session bound %v", path, got, a.sessionLifetime)
		}
		if !isSessionRoute(path) {
			t.Errorf("%s does not count against the session budget", path)
		}
	}
}

func TestRequestAdmissionRejectsWebSocketBeforeUpgrade(t *testing.T) {
	a := newRequestAdmission(1, 1, 1, 4, time.Minute, time.Hour)
	release, status := a.acquire("occupied", "")
	if status != 0 {
		t.Fatal("failed to occupy admission slot")
	}
	defer release()
	srv := httptest.NewServer(a.wrap(http.HandlerFunc(func(http.ResponseWriter, *http.Request) {
		t.Fatal("rejected WebSocket reached handler")
	}), nil, ""))
	defer srv.Close()
	ctx, cancel := context.WithTimeout(context.Background(), time.Second)
	defer cancel()
	_, res, err := websocket.Dial(ctx, "ws"+strings.TrimPrefix(srv.URL, "http"), nil)
	if err == nil {
		t.Fatal("saturated WebSocket upgrade succeeded")
	}
	if res == nil || res.StatusCode != http.StatusServiceUnavailable || res.Header.Get("Retry-After") != "1" {
		t.Fatalf("upgrade response = %#v, want 503 with Retry-After 1", res)
	}
}

type deadlineEndpoint struct{}

func (deadlineEndpoint) ID() string { return "deadline" }
func (deadlineEndpoint) Handle(s transport.Session) error {
	<-s.Context().Done()
	return nil
}

func TestRequestAdmissionBoundsWebSocketLifetime(t *testing.T) {
	e := &endpoints{
		ping:      deadlineEndpoint{},
		admission: newRequestAdmission(1, 1, 1, 4, 20*time.Millisecond, time.Hour),
	}
	srv := httptest.NewServer(listenerMux(context.Background(), e, muxTopology{latency: true}))
	defer srv.Close()
	ctx, cancel := context.WithTimeout(context.Background(), time.Second)
	defer cancel()
	conn, _, err := websocket.Dial(ctx, "ws"+strings.TrimPrefix(srv.URL, "http")+"/ws/ping", nil)
	if err != nil {
		t.Fatal(err)
	}
	defer conn.CloseNow()
	if _, _, err := conn.Read(ctx); websocket.CloseStatus(err) != websocket.StatusNormalClosure {
		t.Fatalf("WebSocket lifetime close = %v", err)
	}
}
