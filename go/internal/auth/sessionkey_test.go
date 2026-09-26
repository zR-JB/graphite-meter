package auth_test

import (
	"net/http"
	"net/http/httptest"
	"net/netip"
	"slices"
	"testing"

	"github.com/zR-JB/graphite-meter/go/internal/auth"
)

// A login is one budget key under its subject's wider one; else the address keys, or none if ambiguous.
func TestBudgetKeysFollowThePrincipalThenTheAddress(t *testing.T) {
	trusted := []netip.Prefix{netip.MustParsePrefix("10.0.0.0/8")}
	anonymous := httptest.NewRequest(http.MethodGet, "/wt/download", nil)
	anonymous.RemoteAddr = "10.0.0.2:1234"
	anonymous.Header.Set("X-Real-IP", "2001:db8::7")
	want := []string{"2001:db8::/64", "2001:db8::/56", "2001:db8::/48"}
	if got, ok := auth.ClientKeys(anonymous, trusted); !ok || !slices.Equal(got, want) {
		t.Fatalf("anonymous keys = %q, %t, want %q", got, ok, want)
	}
	ambiguous := anonymous.Clone(t.Context())
	ambiguous.Header.Add("X-Real-IP", "2001:db8::8")
	if keys, ok := auth.ClientKeys(ambiguous, trusted); ok || keys != nil {
		t.Fatalf("ambiguous evidence keyed as %q", keys)
	}
	r := auth.RequestWithLogin(anonymous, "user-1", "login-a")
	other := auth.RequestWithLogin(anonymous, "user-1", "login-b")
	client, _ := auth.ClientKeys(r, trusted)
	otherKeys, _ := auth.ClientKeys(other, trusted)
	if !slices.Equal(client, []string{"login:login-a", "principal:user-1"}) || client[0] == otherKeys[0] ||
		client[1] != otherKeys[1] {
		t.Fatalf("logins of one subject keyed %q and %q", client, otherKeys)
	}
}
