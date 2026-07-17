package server

import (
	"context"
	"net/http"
	"net/http/httptest"
	"sync"
	"testing"
	"time"
)

func TestRequestAdmissionPerClientAndRelease(t *testing.T) {
	a := newRequestAdmission(3, 2, time.Minute)
	entered := make(chan struct{}, 2)
	release := make(chan struct{})
	h := a.wrap(http.HandlerFunc(func(http.ResponseWriter, *http.Request) {
		entered <- struct{}{}
		<-release
	}), nil)

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
		t.Fatalf("rejection = %d headers %v", w.Code, w.Header())
	}
	close(release)
	wg.Wait()
	w = httptest.NewRecorder()
	h.ServeHTTP(w, httptest.NewRequest(http.MethodGet, "/download", nil))
	if w.Code != http.StatusOK {
		t.Fatalf("request after release = %d", w.Code)
	}
}

func TestRequestAdmissionGlobalLimit(t *testing.T) {
	a := newRequestAdmission(1, 1, time.Minute)
	release, status := a.acquire("192.0.2.1")
	if status != 0 {
		t.Fatal("first request rejected")
	}
	defer release()
	if _, status := a.acquire("192.0.2.2"); status != http.StatusServiceUnavailable {
		t.Fatalf("global rejection = %d", status)
	}
}

func TestClientKeyGroupsIPv6ByPrefix(t *testing.T) {
	a := httptest.NewRequest(http.MethodGet, "/", nil)
	b := httptest.NewRequest(http.MethodGet, "/", nil)
	a.RemoteAddr = "[2001:db8:1::1]:1"
	b.RemoteAddr = "[2001:db8:1::ffff]:2"
	if clientKey(a, nil) != clientKey(b, nil) {
		t.Fatalf("same /64 produced %q and %q", clientKey(a, nil), clientKey(b, nil))
	}
}

func TestRequestAdmissionLifetime(t *testing.T) {
	a := newRequestAdmission(1, 1, 10*time.Millisecond)
	done := make(chan struct{})
	h := a.wrap(http.HandlerFunc(func(_ http.ResponseWriter, r *http.Request) {
		<-r.Context().Done()
		close(done)
	}), nil)
	h.ServeHTTP(httptest.NewRecorder(), httptest.NewRequestWithContext(context.Background(), http.MethodGet, "/", nil))
	select {
	case <-done:
	default:
		t.Fatal("handler did not observe lifetime deadline")
	}
}
