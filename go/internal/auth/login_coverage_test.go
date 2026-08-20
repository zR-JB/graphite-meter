package auth

import (
	"crypto/tls"
	"net/http"
	"net/http/httptest"
	"net/url"
	"strings"
	"testing"
)

// passwordPost builds a valid-CSRF password sign-in request for password mode.
func passwordPost(s *Service, password string) *http.Request {
	token := "abcdefghijklmnopqrstuvwxyz0123456789"
	form := url.Values{"csrf": {token}, "password": {password}}.Encode()
	r := httptest.NewRequest(http.MethodPost, s.public.String()+"/auth/password", strings.NewReader(form))
	r.Host = "meter.example"
	r.TLS = &tls.ConnectionState{}
	r.Header.Set("Content-Type", "application/x-www-form-urlencoded")
	r.Header.Set("Origin", s.public.String())
	r.AddCookie(&http.Cookie{Name: loginCookie, Value: token})
	return r
}

func passwordError(t *testing.T, s *Service, r *http.Request) notice {
	t.Helper()
	rr := httptest.NewRecorder()
	s.passwordLogin(rr, r)
	if rr.Code != http.StatusSeeOther {
		t.Fatalf("code=%d, want 303 redirect", rr.Code)
	}
	loc, err := url.Parse(rr.Header().Get("Location"))
	if err != nil {
		t.Fatal(err)
	}
	return notice(loc.Query().Get("error"))
}

func TestPasswordLoginWrongPassword(t *testing.T) {
	s := testService(t)
	if got := passwordError(t, s, passwordPost(s, "wrong")); got != noticePassword {
		t.Fatalf("wrong password error=%q, want %q", got, noticePassword)
	}
}

func TestPasswordLoginThrottled(t *testing.T) {
	s := testService(t)
	// Spend the per-address budget, then the next attempt is refused without
	// ever reaching the hash.
	for i := range maxAddressAttempts {
		if !s.allowAttempt(passwordPost(s, "secret")) {
			t.Fatalf("attempt %d refused early", i)
		}
	}
	if got := passwordError(t, s, passwordPost(s, "secret")); got != noticeThrottled {
		t.Fatalf("throttled error=%q, want %q", got, noticeThrottled)
	}
}

func TestPasswordLoginVerifierBusy(t *testing.T) {
	s := testService(t)
	// Saturate the Argon2 concurrency gate so verification is refused rather
	// than queued.
	for i := 0; i < cap(s.argon); i++ {
		s.argon <- struct{}{}
	}
	if got := passwordError(t, s, passwordPost(s, "secret")); got != noticeBusy {
		t.Fatalf("verifier-busy error=%q, want %q", got, noticeBusy)
	}
}

func TestPasswordLoginWrongModeIsNotFound(t *testing.T) {
	s := newFakeOIDC(t).service(t)
	rr := httptest.NewRecorder()
	s.passwordLogin(rr, passwordPost(s, "secret"))
	if rr.Code != http.StatusNotFound {
		t.Fatalf("password login in oidc-only mode code=%d, want 404", rr.Code)
	}
}

func TestOIDCStartProviderNotReady(t *testing.T) {
	// In password mode the OIDC provider is absent, so a start request fails
	// closed with the provider-unavailable notice.
	s := testService(t)
	r := httptest.NewRequest(http.MethodPost, s.public.String()+"/auth/oidc/start", strings.NewReader("csrf=x"))
	r.Host = "meter.example"
	r.TLS = &tls.ConnectionState{}
	r.Header.Set("Content-Type", "application/x-www-form-urlencoded")
	r.Header.Set("Origin", s.public.String())
	rr := httptest.NewRecorder()
	s.oidcStart(rr, r)
	loc, _ := url.Parse(rr.Header().Get("Location"))
	if got := loc.Query().Get("error"); got != string(noticeProvider) {
		t.Fatalf("oidc start without a provider error=%q, want %q", got, noticeProvider)
	}
}

func TestOIDCStartRejectsBadCSRF(t *testing.T) {
	// A ready provider still refuses a start request whose double-submit CSRF
	// token has no matching login cookie, redirecting with the stale notice.
	s := newFakeOIDC(t).service(t)
	r := httptest.NewRequest(http.MethodPost, s.public.String()+"/auth/oidc/start", strings.NewReader("csrf=nope"))
	r.Host = "meter.example"
	r.TLS = &tls.ConnectionState{}
	r.Header.Set("Content-Type", "application/x-www-form-urlencoded")
	r.Header.Set("Origin", s.public.String())
	rr := httptest.NewRecorder()
	s.oidcStart(rr, r)
	if rr.Code != http.StatusSeeOther {
		t.Fatalf("code=%d, want 303 redirect", rr.Code)
	}
	loc, _ := url.Parse(rr.Header().Get("Location"))
	if got := loc.Query().Get("error"); got != string(noticeStale) {
		t.Fatalf("bad-csrf oidc start error=%q, want %q", got, noticeStale)
	}
}
