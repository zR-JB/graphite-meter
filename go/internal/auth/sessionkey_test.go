package auth_test

import (
	"net/http"
	"net/http/httptest"
	"testing"

	"github.com/zR-JB/graphite-meter/go/internal/auth"
	"github.com/zR-JB/graphite-meter/go/internal/endpoint"
)

func TestSessionKeyUsesTheLoginNotTheSubject(t *testing.T) {
	r := httptest.NewRequest(http.MethodGet, "/wt/download", nil)
	r.RemoteAddr = "192.0.2.7:1234"
	r = auth.RequestWithLogin(r, "user-1", "login-a")

	if got, want := endpoint.SessionKey(r, nil), "login:login-a"; got != want {
		t.Fatalf("session key = %q, want %q", got, want)
	}
	if got := endpoint.SessionKey(r, nil); got == endpoint.ClientKey(r, nil) {
		t.Fatalf("session key %q collapsed onto the client key: one device's held sessions would starve the same user's others", got)
	}

	other := auth.RequestWithLogin(httptest.NewRequest(http.MethodGet, "/wt/download", nil), "user-1", "login-b")
	if endpoint.SessionKey(r, nil) == endpoint.SessionKey(other, nil) {
		t.Fatal("two logins of one subject share a session budget")
	}
	if endpoint.ClientKey(r, nil) != endpoint.ClientKey(other, nil) {
		t.Fatal("the two logins are not the same subject, so this no longer covers the distinction")
	}
}
