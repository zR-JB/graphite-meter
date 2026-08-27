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
	"github.com/zR-JB/graphite-meter/go/internal/static"
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
	for range 2 {
		wg.Go(func() {
			r := httptest.NewRequest(http.MethodGet, "/download", nil)
			r.RemoteAddr = "192.0.2.10:1234"
			h.ServeHTTP(httptest.NewRecorder(), r)
		})
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

func assertAdmissionLifetime(t *testing.T, path string, requestLifetime, sessionLifetime time.Duration) {
	t.Helper()
	a := newRequestAdmission(1, 1, 1, 4, requestLifetime, sessionLifetime)
	done := make(chan struct{})
	h := a.wrap(http.HandlerFunc(func(_ http.ResponseWriter, r *http.Request) {
		<-r.Context().Done()
		close(done)
	}), nil, "")
	h.ServeHTTP(httptest.NewRecorder(), httptest.NewRequestWithContext(t.Context(), http.MethodGet, path, nil))
	select {
	case <-done:
	default:
		t.Fatalf("handler did not observe %s lifetime deadline", path)
	}
}

func TestRequestAdmissionLifetime(t *testing.T) {
	assertAdmissionLifetime(t, "/", 10*time.Millisecond, time.Hour)
}

func TestRequestAdmissionSessionRouteUsesSessionLifetime(t *testing.T) {
	assertAdmissionLifetime(t, "/wt/download", time.Minute, 10*time.Millisecond)
}

// Session routes carry their own per-client budget, since one holds a slot for a whole test rather than a request.
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

// Session routes carry their own share of the global pool as well as their own per-client bucket.
func TestRequestAdmissionBoundsSessionsGlobally(t *testing.T) {
	// Room for ten measurements and ten sessions per client, but only two sessions overall.
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
	// The property the single global counter lost: the pool still admits the request-shaped routes while every session.
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
	// The refusal came from the session budget with the pool half empty, so it is counted there.
	if stats := a.stats(); stats.active != 0 || stats.rejectedSessionBudget != 1 || stats.rejectedGlobal != 0 {
		t.Fatalf("stats = %+v, want no active measurements and 1 session-budget rejection", stats)
	}
}

// The session budget caps what sessions may occupy and reserves nothing for them.
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

// A full pool and a full session budget both answer 503 and are raised with different knobs.
func TestAdmissionStatsSeparateTheSessionBudgetFromThePool(t *testing.T) {
	// Room for four measurements but only one session.
	a := newRequestAdmission(4, 4, 1, 4, time.Minute, time.Hour)
	session, status := a.acquire("client-a", "login-a")
	if status != 0 {
		t.Fatalf("first session rejected with %d", status)
	}
	defer session()
	if _, status := a.acquire("client-b", "login-b"); status != http.StatusServiceUnavailable {
		t.Fatalf("session past the budget = %d, want %d", status, http.StatusServiceUnavailable)
	}
	stats := a.stats()
	if stats.rejectedSessionBudget != 1 || stats.rejectedGlobal != 0 {
		t.Errorf("stats = %+v, want the refusal counted against the session budget and not the pool", stats)
	}
	if stats.activeSessions != 1 || stats.sessionMax != 1 {
		t.Errorf("stats = %+v, want the session budget reported as 1 of 1 occupied", stats)
	}

	// Fill the rest of the pool with request-shaped routes and prove the other counter is the one that moves.
	for i := range 3 {
		release, status := a.acquire("client-c", "")
		if status != 0 {
			t.Fatalf("request %d rejected with %d while the pool had room", i, status)
		}
		defer release()
	}
	if _, status := a.acquire("client-d", ""); status != http.StatusServiceUnavailable {
		t.Fatalf("request against a full pool = %d, want %d", status, http.StatusServiceUnavailable)
	}
	if stats := a.stats(); stats.rejectedGlobal != 1 || stats.rejectedSessionBudget != 1 {
		t.Errorf("stats = %+v, want one refusal on each counter", stats)
	}
	if stats := a.stats(); stats.active != 4 {
		t.Errorf("stats = %+v, want the pool reported full", stats)
	}
}

// The session budget is per login, not per person.
func TestSessionBudgetIsPerLogin(t *testing.T) {
	// Equal request and session limits are valid.
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

// A principal with no login falls back to the address, so public mode keeps the per-client budget it had before logins.
func TestSessionKeyFallsBackToTheClientKey(t *testing.T) {
	r := httptest.NewRequest(http.MethodGet, "/wt/download", nil)
	r.RemoteAddr = "192.0.2.7:1234"
	if got, want := endpoint.SessionKey(r, nil), endpoint.ClientKey(r, nil); got != want {
		t.Fatalf("session key = %q, want the client key %q", got, want)
	}
}

// The two ping buses are one thing under two mechanisms: neither holds a test.
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

// deadlineRecordingWriter counts the socket deadlines wrap arms through http.NewResponseController.
type deadlineRecordingWriter struct {
	*httptest.ResponseRecorder
	armed int
}

func (w *deadlineRecordingWriter) SetReadDeadline(time.Time) error  { w.armed++; return nil }
func (w *deadlineRecordingWriter) SetWriteDeadline(time.Time) error { w.armed++; return nil }

// A socket deadline bounds a request; it tears a channel down mid-stream.
func TestChannelRoutesTakeNoSocketDeadline(t *testing.T) {
	a := newRequestAdmission(100, 100, 100, 100, time.Minute, time.Hour)
	armedFor := func(path string) int {
		w := &deadlineRecordingWriter{ResponseRecorder: httptest.NewRecorder()}
		a.wrap(http.HandlerFunc(func(http.ResponseWriter, *http.Request) {}), nil, "").
			ServeHTTP(w, httptest.NewRequest(http.MethodGet, path, nil))
		return w.armed
	}
	for _, path := range []string{routePing, routeWTPing, routeWTDownload, routeWTUpload} {
		if got := armedFor(path); got != 0 {
			t.Errorf("%s armed %d socket deadlines, want none: it holds a channel open rather than answering a request", path, got)
		}
	}
	// The control: a request-shaped route still gets its deadlines.
	for _, path := range []string{routeDownload, routeUpload} {
		if got := armedFor(path); got == 0 {
			t.Errorf("%s armed no socket deadline, want one: an unbounded transfer that stops reading its context has nothing else to stop it", path)
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
	ctx, cancel := context.WithTimeout(t.Context(), time.Second)
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
	srv := httptest.NewServer(listenerMuxConfigured(t.Context(), e, muxTopology{latency: true}, static.Handler(), nil))
	defer srv.Close()
	ctx, cancel := context.WithTimeout(t.Context(), time.Second)
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
