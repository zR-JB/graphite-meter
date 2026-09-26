package auth_test

import (
	"net/http"
	"net/http/httptest"
	"testing"

	"github.com/zR-JB/graphite-meter/go/internal/auth"
	"github.com/zR-JB/graphite-meter/go/internal/endpoint"
)

// Two logins of one subject hold separate session budgets; without a login the budget falls back to the client key.
func TestSessionKeyUsesTheLoginNotTheSubject(t *testing.T) {
	sessionKey := func(r *http.Request) string { return endpoint.SessionKey(r, endpoint.ClientKey(r, nil)) }
	anonymous := httptest.NewRequest(http.MethodGet, "/wt/download", nil)
	anonymous.RemoteAddr = "192.0.2.7:1234"
	if got := sessionKey(anonymous); got != "192.0.2.7" {
		t.Fatalf("anonymous session key = %q, want the client key", got)
	}
	r := auth.RequestWithLogin(anonymous, "user-1", "login-a")
	other := auth.RequestWithLogin(anonymous, "user-1", "login-b")
	if got, want := sessionKey(r), "login:login-a"; got != want {
		t.Fatalf("session key = %q, want %q", got, want)
	}
	if sessionKey(r) == sessionKey(other) || endpoint.ClientKey(r, nil) != endpoint.ClientKey(other, nil) {
		t.Fatal("two logins of one subject must share a client key and hold separate session budgets")
	}
}
