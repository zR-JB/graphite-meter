package auth

import (
	"bytes"
	"log"
	"net/http"
	"net/netip"
	"os"
	"strings"
	"testing"
	"testing/synctest"
	"time"
)

func requestFrom(method, path, remote string) *http.Request {
	r := secureRequest(method, path, nil)
	r.RemoteAddr = remote
	return r
}

func addressFrom(i int) string {
	return netip.AddrPortFrom(netip.AddrFrom4([4]byte{10, byte(i >> 16), byte(i >> 8), byte(i)}), 40000).String()
}

func TestAddressBudgets(t *testing.T) {
	for _, tc := range []struct {
		name  string
		limit int
		allow func(*Service, *http.Request) bool
	}{
		{"password", maxAddressAttempts, (*Service).allowAttempt},
		{"exchange", maxAddressExchanges, (*Service).allowExchange},
		{"approval", maxAddressApprovals, (*Service).allowBrowserApproval},
	} {
		t.Run(tc.name, func(t *testing.T) {
			synctest.Test(t, func(t *testing.T) {
				s := testService(t)
				allow := func(remote string) bool { return tc.allow(s, requestFrom(http.MethodPost, "/", remote)) }
				for i := range tc.limit {
					if !allow("[2001:db8:1:2::1]:40000") {
						t.Fatalf("attempt %d was refused", i)
					}
					time.Sleep(time.Second)
				}
				if allow("[2001:db8:1:2::dead:beef]:40000") {
					t.Fatal("a /64 sibling was granted its own budget")
				}
				if !allow("[2001:db8:1:3::1]:40000") || !allow("198.51.100.9:40000") {
					t.Fatal("an unrelated address was refused")
				}
				time.Sleep(attemptWindow - time.Duration(tc.limit)*time.Second)
				if !allow("[2001:db8:1:2::2]:40000") || allow("[2001:db8:1:2::2]:40000") {
					t.Fatal("the rolling window did not release exactly the oldest attempt")
				}
			})
		})
	}
}

func TestPasswordCeilingIsGlobalAndLogsOncePerWindow(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		s := testService(t)
		for i := range 61 {
			if got := s.allowAttempt(requestFrom(http.MethodPost, "/auth/password", "192.0.2.1:1234")); got != (i < 5) {
				t.Fatalf("attempt %d allowed=%v", i+1, got)
			}
		}
		if len(s.globalAttempts) != maxAddressAttempts {
			t.Fatalf("per-address refusals spent the global budget: %d", len(s.globalAttempts))
		}
		var out bytes.Buffer
		log.SetOutput(&out)
		t.Cleanup(func() { log.SetOutput(os.Stderr) })
		spend := func() {
			for i := range maxGlobalAttempts + 20 {
				s.allowAttempt(requestFrom(http.MethodPost, "/auth/password", addressFrom(i%200)))
			}
		}
		for _, want := range []int{1, 1} {
			spend()
			if got := strings.Count(out.String(), "ceiling engaged"); got != want {
				t.Fatalf("logged %d ceiling notices in one window, want %d", got, want)
			}
		}
		if s.allowAttempt(requestFrom(http.MethodPost, "/auth/password", "198.51.100.1:1234")) {
			t.Fatal("global password-attempt ceiling was bypassed with a new address")
		}
		time.Sleep(ceilingLogInterval + time.Second)
		spend()
		if got := strings.Count(out.String(), "ceiling engaged"); got != 2 {
			t.Fatalf("logged %d ceiling notices across two windows, want 2", got)
		}
	})
}

func TestAddressStoreStaysBoundedAndExpires(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		s := testService(t)
		for i := range maxBudgetKeys + 100 {
			if allowed := s.allowBrowserApproval(requestFrom(http.MethodGet, "/auth/browser",
				addressFrom(i))); allowed != (i < maxBudgetKeys) {
				t.Fatalf("address %d admitted=%t", i, allowed)
			}
		}
		if len(s.approvalAttempts) != maxBudgetKeys {
			t.Fatalf("store has %d keys, want %d", len(s.approvalAttempts), maxBudgetKeys)
		}
		time.Sleep(attemptWindow + time.Second)
		if !s.allowBrowserApproval(requestFrom(http.MethodGet, "/auth/browser", "203.0.113.5:40000")) ||
			len(s.approvalAttempts) != 1 {
			t.Fatal("expired addresses still occupied the bounded store")
		}
	})
}
