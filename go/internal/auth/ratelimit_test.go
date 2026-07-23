package auth

import (
	"bytes"
	"log"
	"net/http"
	"net/netip"
	"os"
	"strings"
	"testing"
	"time"
)

func requestFrom(method, path, remote string) *http.Request {
	r := secureRequest(method, path, nil)
	r.RemoteAddr = remote
	return r
}

func TestBudgetKeyCollapsesIPv6ToTheAllocation(t *testing.T) {
	for _, tc := range []struct {
		addr, want string
	}{
		{"203.0.113.7", "203.0.113.7"},
		{"::ffff:203.0.113.7", "203.0.113.7"},
		{"2001:db8:1:2::1", "2001:db8:1:2::/64"},
		{"2001:db8:1:2:ffff:ffff:ffff:ffff", "2001:db8:1:2::/64"},
		{"2001:db8:1:3::1", "2001:db8:1:3::/64"},
	} {
		if got := budgetKey(netip.MustParseAddr(tc.addr)); got != tc.want {
			t.Fatalf("budgetKey(%s) = %s, want %s", tc.addr, got, tc.want)
		}
	}
}

// One IPv6 allocation routinely covers 2^64 addresses. Keying budgets per /128
// would let a single customer prefix spend the per-address budget once per
// address it owns, which is no budget at all.
func TestIPv6SiblingsShareOnePasswordBudget(t *testing.T) {
	s := testService(t)
	for i := 0; i < maxAddressAttempts; i++ {
		if !s.allowAttempt(requestFrom(http.MethodPost, "/auth/password", "[2001:db8:1:2::1]:40000")) {
			t.Fatalf("attempt %d from the first address was refused", i)
		}
	}
	if s.allowAttempt(requestFrom(http.MethodPost, "/auth/password", "[2001:db8:1:2::dead:beef]:40000")) {
		t.Fatal("a /64 sibling was granted its own password budget")
	}
	if !s.allowAttempt(requestFrom(http.MethodPost, "/auth/password", "[2001:db8:1:3::1]:40000")) {
		t.Fatal("a different /64 was refused")
	}
}

func TestIPv6SiblingsShareOneExchangeBudget(t *testing.T) {
	s := testService(t)
	for i := 0; i < maxAddressExchanges; i++ {
		if !s.allowExchange(requestFrom(http.MethodGet, "/auth/oidc/callback", "[2001:db8:1:2::1]:40000")) {
			t.Fatalf("exchange %d from the first address was refused", i)
		}
	}
	if s.allowExchange(requestFrom(http.MethodGet, "/auth/oidc/callback", "[2001:db8:1:2::99]:40000")) {
		t.Fatal("a /64 sibling was granted its own exchange budget")
	}
	if !s.allowExchange(requestFrom(http.MethodGet, "/auth/oidc/callback", "[2001:db8:9::1]:40000")) {
		t.Fatal("a different /64 was refused")
	}
}

// The token exchange is the only anonymous path that produces an outbound
// request to the identity provider, so it needs a volume bound of its own —
// the transaction caps free on use and bound only concurrency.
func TestTokenExchangeIsThrottledPerAddress(t *testing.T) {
	s := testService(t)
	address := "203.0.113.7:40000"
	for i := 0; i < maxAddressExchanges; i++ {
		if !s.allowExchange(requestFrom(http.MethodGet, "/auth/oidc/callback", address)) {
			t.Fatalf("exchange %d was refused", i)
		}
	}
	if s.allowExchange(requestFrom(http.MethodGet, "/auth/oidc/callback", address)) {
		t.Fatal("exchange budget is not enforced")
	}
	if !s.allowExchange(requestFrom(http.MethodGet, "/auth/oidc/callback", "198.51.100.9:40000")) {
		t.Fatal("an unrelated address was refused")
	}
}

func TestExchangeBudgetDrainsWithTheWindow(t *testing.T) {
	s := testService(t)
	now := time.Now()
	s.now = func() time.Time { return now }
	address := "203.0.113.7:40000"
	for i := 0; i < maxAddressExchanges; i++ {
		s.allowExchange(requestFrom(http.MethodGet, "/auth/oidc/callback", address))
	}
	if s.allowExchange(requestFrom(http.MethodGet, "/auth/oidc/callback", address)) {
		t.Fatal("exchange budget is not enforced")
	}
	now = now.Add(attemptWindow + time.Second)
	if !s.allowExchange(requestFrom(http.MethodGet, "/auth/oidc/callback", address)) {
		t.Fatal("exchange budget did not drain")
	}
}

// A saturated global ceiling and a broken service look the same from outside.
// The log line is how an operator tells them apart, and it must not itself
// become a flood an attacker can drive.
func TestGlobalCeilingLogsOncePerWindow(t *testing.T) {
	s := testService(t)
	now := time.Now()
	s.now = func() time.Time { return now }
	var out bytes.Buffer
	log.SetOutput(&out)
	t.Cleanup(func() { log.SetOutput(os.Stderr) })

	spend := func() {
		for i := 0; i < maxGlobalAttempts+20; i++ {
			s.allowAttempt(requestFrom(http.MethodPost, "/auth/password", netip.AddrPortFrom(netip.AddrFrom4([4]byte{203, 0, 113, byte(i % 200)}), 40000).String()))
		}
	}
	spend()
	if got := strings.Count(out.String(), "ceiling engaged"); got != 1 {
		t.Fatalf("logged %d ceiling notices in one window, want 1", got)
	}
	spend()
	if got := strings.Count(out.String(), "ceiling engaged"); got != 1 {
		t.Fatalf("logged %d ceiling notices while still inside the window, want 1", got)
	}
	now = now.Add(ceilingLogInterval + time.Second)
	spend()
	if got := strings.Count(out.String(), "ceiling engaged"); got != 2 {
		t.Fatalf("logged %d ceiling notices across two windows, want 2", got)
	}
}

func TestAttemptStoreStaysBounded(t *testing.T) {
	s := testService(t)
	now := time.Now()
	s.now = func() time.Time { return now }
	for i := 0; i < maxBudgetKeys+100; i++ {
		s.allowExchange(requestFrom(http.MethodGet, "/auth/oidc/callback", netip.AddrPortFrom(netip.AddrFrom4([4]byte{10, byte(i >> 16), byte(i >> 8), byte(i)}), 40000).String()))
	}
	s.mu.Lock()
	size := len(s.exchanges)
	s.mu.Unlock()
	if size > maxBudgetKeys {
		t.Fatalf("exchange store grew to %d keys, want at most %d", size, maxBudgetKeys)
	}
}
