package auth

import (
	"net/http"
	"net/http/httptest"
	"net/url"
	"strings"
	"testing"
)

// credentialOutcomes must stay generic: telling them apart reveals another
// identity's authorization state (the OIDC group, subject, and signature
// outcomes) or an attack indicator (CSRF). The operator password and rate
// limiting are absent by design, they say so plainly.
var credentialOutcomes = []reason{
	reasonCSRFOriginMissing,
	reasonCSRFOriginMismatch,
	reasonCSRFTokenMismatch,
	reasonFormMalformed,
	reasonClientAddress,
	reasonExchangeRateLimited,
	reasonCallbackParameters,
	reasonTransactionReplay,
	reasonResponseIssuer,
	reasonTokenExchange,
	reasonMissingIDToken,
	reasonIDTokenVerification,
	reasonIDTokenClaimsOrNonce,
	reasonAccessTokenHash,
	reasonUserInfoOrSubject,
	reasonUserInfoClaimsOrGroup,
	reasonInvalidSubject,
}

func TestSafeNoticeSubsetExcludesCredentialOutcomes(t *testing.T) {
	for _, why := range credentialOutcomes {
		if got := noticeFor(why); got != noticeGeneric {
			t.Fatalf("reason %q leaks notice %q", why, got)
		}
		if _, ok := reasonNotices[why]; ok {
			t.Fatalf("reason %q must not be in the safe subset", why)
		}
	}
}

func TestSafeNoticesDescribeServerOrFormStateOnly(t *testing.T) {
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
	if len(reasonNotices) != len(want) {
		t.Fatalf("safe subset has %d entries, want %d; new entries need a security review", len(reasonNotices), len(want))
	}
	for why, n := range want {
		if got := noticeFor(why); got != n {
			t.Fatalf("reason %q = notice %q, want %q", why, got, n)
		}
	}
}

func TestParseNoticeAcceptsOnlyKnownCodes(t *testing.T) {
	for _, tc := range []struct {
		raw  string
		want notice
	}{
		{"", ""},
		{"provider", noticeProvider},
		{"busy", noticeBusy},
		{"stale", noticeStale},
		{"throttled", noticeThrottled},
		{"password", noticePassword},
		{"failed", noticeGeneric},
		{"1", noticeGeneric},
		{"<script>alert(1)</script>", noticeGeneric},
	} {
		if got := parseNotice(tc.raw); got != tc.want {
			t.Fatalf("parseNotice(%q) = %q, want %q", tc.raw, got, tc.want)
		}
	}
}

func TestLoginRejectedCarriesOnlyTheSafeNotice(t *testing.T) {
	s := testService(t)
	for _, tc := range []struct {
		why  reason
		want notice
	}{
		{reasonPasswordMismatch, noticePassword},
		{reasonThrottled, noticeThrottled},
		{reasonUserInfoClaimsOrGroup, noticeGeneric},
		{reasonProviderNotReady, noticeProvider},
		{reasonSessionCapacity, noticeBusy},
		{reasonCSRFCookieMissing, noticeStale},
	} {
		r := secureRequest(http.MethodPost, "/auth/password", nil)
		r.Form = url.Values{}
		rr := httptest.NewRecorder()
		s.loginRejected(rr, r, tc.why)
		location, err := url.Parse(rr.Header().Get("Location"))
		if err != nil {
			t.Fatal(err)
		}
		if got := location.Query().Get("error"); got != string(tc.want) {
			t.Fatalf("reason %q redirected with error=%q, want %q", tc.why, got, tc.want)
		}
		if strings.Contains(rr.Header().Get("Location"), string(tc.why)) && tc.want == noticeGeneric {
			t.Fatalf("reason %q leaked into the redirect", tc.why)
		}
	}
}

// loginAlerts returns the alert paragraphs the login page rendered, with the
// per-render CSRF token excluded by construction.
func loginAlerts(t *testing.T, s *Service, code string) []string {
	t.Helper()
	r := secureRequest(http.MethodGet, "/login?error="+url.QueryEscape(code), nil)
	rr := httptest.NewRecorder()
	s.loginPage(rr, r)
	var alerts []string
	// A failure/limit notice is a <p ... role="alert"> regardless of its class
	// (message, warn, notice); the expected-state banners use role="status".
	for _, part := range strings.Split(rr.Body.String(), `role="alert">`)[1:] {
		alerts = append(alerts, strings.SplitN(part, "</p>", 2)[0])
	}
	return alerts
}

func TestLoginPageRendersEachNoticeDistinctly(t *testing.T) {
	s := testService(t)
	seen := map[string]string{}
	for _, code := range []string{"", "failed", "provider", "busy", "stale", "throttled", "password"} {
		alerts := loginAlerts(t, s, code)
		if code == "" {
			if len(alerts) != 0 {
				t.Fatalf("clean login page rendered alerts: %q", alerts)
			}
			continue
		}
		if len(alerts) != 1 {
			t.Fatalf("error=%q rendered %d alerts, want 1", code, len(alerts))
		}
		if prior, ok := seen[alerts[0]]; ok {
			t.Fatalf("error=%q renders identically to error=%q", code, prior)
		}
		seen[alerts[0]] = code
	}
}

func TestUnknownNoticeCodeCollapsesToGeneric(t *testing.T) {
	s := testService(t)
	generic := loginAlerts(t, s, "failed")
	for _, code := range []string{"1", "provider ", "PROVIDER", "<img src=x>", "busy;drop"} {
		got := loginAlerts(t, s, code)
		if len(got) != len(generic) || got[0] != generic[0] {
			t.Fatalf("error=%q rendered %q, want the generic notice %q", code, got, generic)
		}
	}
}
