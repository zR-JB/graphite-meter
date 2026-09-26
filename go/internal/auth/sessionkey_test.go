package auth_test

import (
	"net/http"
	"net/http/httptest"
	"net/netip"
	"slices"
	"testing"

	"github.com/zR-JB/graphite-meter/go/internal/auth"
)

// A principal is one budget key and each of its logins one session key; an address falls back to its keys,
// and a trusted proxy's ambiguous evidence has none.
func TestBudgetKeysFollowThePrincipalThenTheAddress(t *testing.T) {
	trusted := []netip.Prefix{netip.MustParsePrefix("10.0.0.0/8")}
	anonymous := httptest.NewRequest(http.MethodGet, "/wt/download", nil)
	anonymous.RemoteAddr = "10.0.0.2:1234"
	anonymous.Header.Set("X-Real-IP", "2001:db8::7")
	want := []string{"2001:db8::/64", "2001:db8::/56", "2001:db8::/48"}
	for _, keys := range []func(*http.Request, []netip.Prefix) ([]string, bool){auth.ClientKeys, auth.SessionKeys} {
		if got, ok := keys(anonymous, trusted); !ok || !slices.Equal(got, want) {
			t.Fatalf("anonymous keys = %q, %t, want %q", got, ok, want)
		}
	}
	ambiguous := anonymous.Clone(t.Context())
	ambiguous.Header.Add("X-Real-IP", "2001:db8::8")
	if keys, ok := auth.ClientKeys(ambiguous, trusted); ok || keys != nil {
		t.Fatalf("ambiguous evidence keyed as %q", keys)
	}
	r := auth.RequestWithLogin(anonymous, "user-1", "login-a")
	other := auth.RequestWithLogin(anonymous, "user-1", "login-b")
	client, _ := auth.ClientKeys(r, trusted)
	otherClient, _ := auth.ClientKeys(other, trusted)
	session, _ := auth.SessionKeys(r, trusted)
	otherSession, _ := auth.SessionKeys(other, trusted)
	if !slices.Equal(client, []string{"principal:user-1"}) || !slices.Equal(client, otherClient) ||
		!slices.Equal(session, []string{"login:login-a"}) || slices.Equal(session, otherSession) {
		t.Fatalf("logins of one subject keyed %q/%q and sessions %q/%q", client, otherClient, session, otherSession)
	}
}
