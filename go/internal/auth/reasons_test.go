package auth

import (
	"crypto/tls"
	"net/http"
	"net/http/httptest"
	"net/url"
	"strings"
	"testing"
)

// Every refusal reason reaches the browser as a notice describing server or
// form state; a credential outcome is never distinguishable from a generic failure.
func TestLoginRefusalsCarryOnlyASafeNotice(t *testing.T) {
	want := map[reason]notice{
		reasonProviderNotReady:    noticeProvider,
		reasonVerifierBusy:        noticeBusy,
		reasonSessionCapacity:     noticeBusy,
		reasonTransactionCapacity: noticeBusy,
		reasonThrottled:           noticeThrottled,
		reasonPasswordMismatch:    noticePassword,
		reasonCSRFCookieMissing:   noticeStale,
		reasonCSRFTokenMissing:    noticeStale,
		reasonTransactionCookie:   noticeStale,
	}
	for _, why := range []reason{
		reasonCSRFOriginMissing, reasonCSRFOriginMismatch, reasonCSRFTokenMismatch, reasonFormMalformed,
		reasonClientAddress, reasonExchangeRateLimited, reasonCallbackParameters, reasonTransactionReplay,
		reasonResponseIssuer, reasonTokenExchange, reasonMissingIDToken, reasonIDTokenVerification,
		reasonIDTokenClaimsOrNonce, reasonAccessTokenHash, reasonUserInfoOrSubject, reasonUserInfoClaimsOrGroup,
		reasonInvalidSubject,
	} {
		want[why] = noticeGeneric
	}
	s := testService(t)
	for why, n := range want {
		r := secureRequest(http.MethodPost, "/auth/password", nil)
		r.Form = url.Values{}
		rr := httptest.NewRecorder()
		s.loginRejected(rr, r, why)
		location := rr.Header().Get("Location")
		if got := noticeFor(why); got != n || !strings.HasSuffix(location, "?error="+string(n)) {
			t.Errorf("reason %q = notice %q redirecting to %q, want %q", why, got, location, n)
		}
	}
	for raw, n := range map[string]notice{
		"": "", "provider": noticeProvider, "busy": noticeBusy, "stale": noticeStale, "throttled": noticeThrottled,
		"password": noticePassword, "failed": noticeGeneric, "1": noticeGeneric, "<script>alert(1)</script>": noticeGeneric,
	} {
		if got := parseNotice(raw); got != n {
			t.Errorf("parseNotice(%q) = %q, want %q", raw, got, n)
		}
	}
}

// The sign-in handlers reach those notices through their real refusal paths.
func TestSignInRefusalPaths(t *testing.T) {
	post := func(s *Service, path, form string, withCSRF bool) *http.Request {
		const token = "abcdefghijklmnopqrstuvwxyz0123456789"
		if withCSRF {
			form += "&csrf=" + token
		}
		r := httptest.NewRequest(http.MethodPost, s.origin+path, strings.NewReader(form))
		r.Host, r.TLS = "meter.example", &tls.ConnectionState{}
		r.Header.Set("Content-Type", "application/x-www-form-urlencoded")
		r.Header.Set("Origin", s.origin)
		if withCSRF {
			r.AddCookie(&http.Cookie{Name: loginCookie, Value: token})
		}
		return r
	}
	for _, tc := range []struct {
		name    string
		service func(*testing.T) *Service
		prepare func(*Service)
		request func(*Service) *http.Request
		handler func(*Service) http.HandlerFunc
		want    notice
	}{
		{"wrong password", testService, nil,
			func(s *Service) *http.Request { return post(s, "/auth/password", "password=wrong", true) },
			func(s *Service) http.HandlerFunc { return s.passwordLogin }, noticePassword},
		{"per-address budget spent", testService, func(s *Service) {
			for range maxAddressAttempts {
				s.allowAttempt(post(s, "/auth/password", "", false))
			}
		}, func(s *Service) *http.Request { return post(s, "/auth/password", "password=secret", true) },
			func(s *Service) http.HandlerFunc { return s.passwordLogin }, noticeThrottled},
		{"verifier saturated", testService, func(s *Service) {
			for range cap(s.argon) {
				s.argon <- struct{}{}
			}
		}, func(s *Service) *http.Request { return post(s, "/auth/password", "password=secret", true) },
			func(s *Service) http.HandlerFunc { return s.passwordLogin }, noticeBusy},
		{"OIDC start without a provider", testService, nil,
			func(s *Service) *http.Request { return post(s, "/auth/oidc/start", "csrf=x", false) },
			func(s *Service) http.HandlerFunc { return s.oidcStart }, noticeProvider},
		{"OIDC start with a bad form token", func(t *testing.T) *Service { return newFakeOIDC(t).service(t) }, nil,
			func(s *Service) *http.Request { return post(s, "/auth/oidc/start", "csrf=nope", false) },
			func(s *Service) http.HandlerFunc { return s.oidcStart }, noticeStale},
	} {
		t.Run(tc.name, func(t *testing.T) {
			s := tc.service(t)
			if tc.prepare != nil {
				tc.prepare(s)
			}
			rr := httptest.NewRecorder()
			tc.handler(s)(rr, tc.request(s))
			location, _ := url.Parse(rr.Header().Get("Location"))
			if rr.Code != http.StatusSeeOther || location.Query().Get("error") != string(tc.want) {
				t.Fatalf("status %d redirecting to %q, want 303 with error=%s", rr.Code, location, tc.want)
			}
		})
	}
	t.Run("password sign-in outside password mode", func(t *testing.T) {
		s := newFakeOIDC(t).service(t)
		rr := httptest.NewRecorder()
		s.passwordLogin(rr, post(s, "/auth/password", "password=secret", true))
		if rr.Code != http.StatusNotFound {
			t.Fatalf("password login in oidc-only mode code=%d, want 404", rr.Code)
		}
	})
}

// Each known notice renders its own alert; anything else collapses to the generic one.
func TestLoginPageRendersOnlyKnownNotices(t *testing.T) {
	s := testService(t)
	alerts := func(code string) []string {
		rr := httptest.NewRecorder()
		s.loginPage(rr, secureRequest(http.MethodGet, "/login?error="+url.QueryEscape(code), nil))
		parts := strings.Split(rr.Body.String(), `role="alert">`)[1:]
		for i, part := range parts {
			parts[i], _, _ = strings.Cut(part, "</p>")
		}
		return parts
	}
	if got := alerts(""); len(got) != 0 {
		t.Fatalf("clean login page rendered alerts: %q", got)
	}
	seen := map[string]string{}
	for _, code := range []string{"failed", "provider", "busy", "stale", "throttled", "password"} {
		got := alerts(code)
		if len(got) != 1 {
			t.Fatalf("error=%q rendered %d alerts, want 1", code, len(got))
		}
		if prior, ok := seen[got[0]]; ok {
			t.Fatalf("error=%q renders identically to error=%q", code, prior)
		}
		seen[got[0]] = code
	}
	generic := alerts("failed")
	for _, code := range []string{"1", "provider ", "PROVIDER", "<img src=x>", "busy;drop"} {
		if got := alerts(code); len(got) != 1 || got[0] != generic[0] {
			t.Fatalf("error=%q rendered %q, want the generic notice %q", code, got, generic)
		}
	}
}
