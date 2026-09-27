package auth

import (
	"bytes"
	"encoding/base64"
	"encoding/binary"
	"fmt"
	"log"
	"net/http"
	"net/http/httptest"
	"net/netip"
	"net/url"
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
		{"oidc start", maxAddressOIDCStarts, (*Service).allowOIDCStart},
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

// One IPv6 /48 cannot spend more than four clients' password budget, however many /64s it spreads across.
func TestOneAllocationHoldsABoundedShareOfThePasswordBudget(t *testing.T) {
	s := testService(t)
	allowed := 0
	for i := range 64 {
		remote := fmt.Sprintf("[2001:db8:0:%x::1]:40000", i<<4)
		if s.allowAttempt(requestFrom(http.MethodPost, "/auth/password", remote)) {
			allowed++
		}
	}
	if allowed != 4*maxAddressAttempts {
		t.Fatalf("one /48 made %d attempts, want %d", allowed, 4*maxAddressAttempts)
	}
	if !s.allowAttempt(requestFrom(http.MethodPost, "/auth/password", "[2001:db8:1::1]:40000")) {
		t.Fatal("another allocation was refused")
	}
}

func TestPasswordCeilingCountsOnlyFailuresAndLogsOncePerWindow(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		s := testService(t)
		for i := range maxGlobalAttempts + 20 {
			if !s.allowAttempt(requestFrom(http.MethodPost, "/auth/password", addressFrom(i%200))) {
				t.Fatalf("attempt %d without a wrong password met the global ceiling", i+1)
			}
		}
		var out bytes.Buffer
		log.SetOutput(&out)
		t.Cleanup(func() { log.SetOutput(os.Stderr) })
		spend := func() {
			for i := range maxGlobalAttempts + 20 {
				if s.allowAttempt(requestFrom(http.MethodPost, "/auth/password", addressFrom(200+i%200))) {
					s.noteFailedPassword()
				}
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

// Other clients can engage the global ceiling or fill the address table; neither locks out a known device.
func TestKnownDeviceSignsInPastOtherClientsAttempts(t *testing.T) {
	for name, spend := range map[string]func(*Service, func(remote, password string)){
		"global ceiling": func(_ *Service, signIn func(string, string)) {
			for i := range maxGlobalAttempts {
				signIn(addressFrom(i), "wrong")
			}
		},
		"full address table": func(s *Service, signIn func(string, string)) {
			for range cap(s.argon) {
				s.argon <- struct{}{}
			}
			for i := range maxBudgetKeys {
				signIn(addressFrom(i), "secret")
			}
			for range cap(s.argon) {
				<-s.argon
			}
		},
	} {
		t.Run(name, func(t *testing.T) {
			t.Parallel()
			s := testService(t)
			mux := http.NewServeMux()
			s.Mount(mux)
			signIn := func(remote, password string, device *http.Cookie) *http.Response {
				const token = "abcdefghijklmnopqrstuvwxyz0123456789"
				form := url.Values{"csrf": {token}, "password": {password}}.Encode()
				r := secureRequest(http.MethodPost, "/auth/password", strings.NewReader(form))
				r.RemoteAddr = remote
				r.Header.Set("Content-Type", "application/x-www-form-urlencoded")
				r.Header.Set("Origin", s.origin)
				r.AddCookie(&http.Cookie{Name: loginCookie, Value: token})
				if device != nil {
					r.AddCookie(device)
				}
				rr := httptest.NewRecorder()
				mux.ServeHTTP(rr, r)
				return rr.Result()
			}
			signedIn := func(res *http.Response) bool { return res.Header.Get("Location") == "/" }
			var device *http.Cookie
			for _, c := range signIn("198.51.100.7:40000", "secret", nil).Cookies() {
				if c.Name == deviceCookie {
					device = c
				}
			}
			if device == nil || !device.HttpOnly || !device.Secure || device.SameSite != http.SameSiteStrictMode {
				t.Fatalf("sign-in issued device cookie %v, want HttpOnly, Secure and SameSite=Strict", device)
			}
			spend(s, func(remote, password string) {
				if signedIn(signIn(remote, password, nil)) && password != "secret" {
					t.Fatal("a wrong password signed in")
				}
			})
			if signedIn(signIn("203.0.113.9:40000", "secret", nil)) {
				t.Fatal("an unknown address signed in past the other clients' attempts")
			}
			if !signedIn(signIn("192.0.2.1:40000", "secret", device)) {
				t.Fatal("a known device was locked out by other clients' attempts")
			}
			raw, _ := base64.RawURLEncoding.DecodeString(device.Value)
			raw[len(raw)-1] ^= 1
			past := time.Now().Add(-time.Minute).Unix()
			expired := append(binary.BigEndian.AppendUint64(nil, uint64(past)), s.deviceTag(past)...)
			for name, value := range map[string][]byte{"forged": raw, "expired": expired} {
				if signedIn(signIn("192.0.2.2:40000", "secret", &http.Cookie{Name: deviceCookie,
					Value: base64.RawURLEncoding.EncodeToString(value)})) {
					t.Fatalf("a %s device cookie skipped the other clients' bounds", name)
				}
			}
		})
	}
}
