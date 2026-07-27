package auth_test

// This file is package auth_test rather than auth: it imports internal/endpoint,
// and endpoint imports auth, so an in-package test file importing it would be an
// import cycle. See export_test.go for why the test cannot live in endpoint or
// server instead.

import (
	"net/http"
	"net/http/httptest"
	"testing"

	"github.com/zR-JB/graphite-meter/go/internal/auth"
	"github.com/zR-JB/graphite-meter/go/internal/endpoint"
)

// The per-client session budget is keyed by login, not by subject. A phone and a
// desktop are two logins of one user, so keying by subject would let whichever
// device parked sessions first starve the same user's others out of the budget
// entirely — and the failure looks like a capacity refusal on a server with
// capacity to spare.
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

	// The same subject on a second login gets its own bucket, which is the whole
	// point: the two devices are budgeted apart.
	other := auth.RequestWithLogin(httptest.NewRequest(http.MethodGet, "/wt/download", nil), "user-1", "login-b")
	if endpoint.SessionKey(r, nil) == endpoint.SessionKey(other, nil) {
		t.Fatal("two logins of one subject share a session budget")
	}
	if endpoint.ClientKey(r, nil) != endpoint.ClientKey(other, nil) {
		t.Fatal("the two logins are not the same subject, so this no longer covers the distinction")
	}
}
