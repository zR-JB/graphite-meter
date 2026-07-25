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
	a := newRequestAdmission(3, 2, time.Minute, time.Hour)
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
	a := newRequestAdmission(1, 1, time.Minute, time.Hour)
	release, status := a.acquire("192.0.2.1", false)
	if status != 0 {
		t.Fatal("first request rejected")
	}
	defer release()
	if _, status := a.acquire("192.0.2.2", false); status != http.StatusServiceUnavailable {
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
	a := newRequestAdmission(1, 1, 10*time.Millisecond, time.Hour)
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
	a := newRequestAdmission(1, 1, time.Minute, 10*time.Millisecond)
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

func TestRequestAdmissionRejectsWebSocketBeforeUpgrade(t *testing.T) {
	a := newRequestAdmission(1, 1, time.Minute, time.Hour)
	release, status := a.acquire("occupied", false)
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
		admission: newRequestAdmission(1, 1, 20*time.Millisecond, time.Hour),
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
