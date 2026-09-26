package auth

import (
	"encoding/json/v2"
	"net/http"
	"net/http/httptest"
	"net/netip"
	"net/url"
	"strings"
	"testing"
	"time"
)

// proxiedService trusts 192.0.2.0/24, the documented reverse-proxy topology.
func proxiedService(t *testing.T) *Service {
	s := testService(t)
	s.trusted = []netip.Prefix{netip.MustParsePrefix("192.0.2.0/24")}
	return s
}

func clearRequest(method, path, remote string) *http.Request {
	r := httptest.NewRequest(method, "http://meter.example"+path, nil)
	r.Host = "meter.example"
	r.RemoteAddr = remote
	return r
}

func TestForwardedHeadersAreEvidenceOnlyFromATrustedPeer(t *testing.T) {
	s := proxiedService(t)
	v := func(values ...string) []string { return values }
	for _, tc := range []struct {
		name, remote string
		proto, host  []string
		trusted      bool
	}{
		{"untrusted peer forging both headers", "198.51.100.9:40000", v("https"), v("meter.example"), false},
		{"no headers", "192.0.2.10:40000", nil, nil, false},
		{"proto only", "192.0.2.10:40000", v("https"), nil, false},
		{"duplicated proto header", "192.0.2.10:40000", v("https", "https"), v("meter.example"), false},
		{"comma-joined proto header", "192.0.2.10:40000", v("https,http"), v("meter.example"), false},
		{"comma-joined host header", "192.0.2.10:40000", v("https"), v("meter.example,evil.example"), false},
		{"foreign host", "192.0.2.10:40000", v("https"), v("evil.example"), false},
		{"http", "192.0.2.10:40000", v("http"), v("meter.example"), false},
		{"documented header pair", "192.0.2.10:40000", v("https"), v("meter.example"), true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			r := clearRequest(http.MethodGet, "/login", tc.remote)
			r.Header["X-Forwarded-Proto"], r.Header["X-Forwarded-Host"] = tc.proto, tc.host
			if got := s.requestTrust(r); got != (trust{Secure: tc.trusted, Canonical: tc.trusted}) {
				t.Fatalf("requestTrust = %+v, want both %t", got, tc.trusted)
			}
			if rr := serveMounted(s, r); (rr.Code == http.StatusOK) != tc.trusted {
				t.Fatalf("login page status = %d, want reachable=%t", rr.Code, tc.trusted)
			}
		})
	}
}

// Sign-in budgets use the one client resolver and refuse a proxied request whose client is ambiguous.
func TestSignInBudgetsFailClosedBehindATrustedProxy(t *testing.T) {
	s := proxiedService(t)
	for _, tc := range []struct {
		remote, realIP, forwardedFor string
		allowed                      bool
	}{
		{"198.51.100.9:40000", "", "", true},
		{"192.0.2.10:40000", "203.0.113.1", "", true},
		{"192.0.2.10:40000", "", "", false},
		{"192.0.2.10:40000", "203.0.113.1", "203.0.113.1", false},
	} {
		r := clearRequest(http.MethodPost, "/auth/password", tc.remote)
		for name, value := range map[string]string{"X-Real-IP": tc.realIP, "X-Forwarded-For": tc.forwardedFor} {
			if value != "" {
				r.Header.Set(name, value)
			}
		}
		if got := s.allowAttempt(r); got != tc.allowed {
			t.Errorf("%+v allowed = %t", tc, got)
		}
	}
}

func TestCookieAttributesSatisfyTheHostPrefix(t *testing.T) {
	expires := time.Now().Add(time.Hour)
	rr := httptest.NewRecorder()
	for _, name := range []string{sessionCookie, csrfCookie, loginCookie} {
		setCookie(rr, name, "value-value-value-value", expires, http.SameSiteStrictMode)
	}
	setCookie(rr, transactionCookie, "value-value-value-value", expires, http.SameSiteLaxMode)
	clearCookie(rr, sessionCookie, http.SameSiteStrictMode)
	want := map[string]struct {
		httpOnly bool
		sameSite http.SameSite
	}{
		sessionCookie: {httpOnly: true, sameSite: http.SameSiteStrictMode},
		loginCookie:   {httpOnly: true, sameSite: http.SameSiteStrictMode},
		// The SPA mirrors this one into X-CSRF-Token, so it is readable by design; it carries no authority on its own.
		csrfCookie:        {httpOnly: false, sameSite: http.SameSiteStrictMode},
		transactionCookie: {httpOnly: true, sameSite: http.SameSiteLaxMode},
	}
	cookies := rr.Result().Cookies()
	for i, c := range cookies {
		expect, ok := want[c.Name]
		if !ok || !strings.HasPrefix(c.Name, "__Host-") || !c.Secure || c.Path != "/" || c.Domain != "" ||
			c.HttpOnly != expect.httpOnly || c.SameSite != expect.sameSite {
			t.Fatalf("cookie %+v breaks the __Host- prefix or its scope", c)
		}
		if cleared := i == len(cookies)-1; cleared != (c.MaxAge < 0) {
			t.Fatalf("cookie %q MaxAge=%d", c.Name, c.MaxAge)
		}
	}
	if len(cookies) != len(want)+1 {
		t.Fatalf("set %d cookies, want %d", len(cookies), len(want)+1)
	}
}

func TestPasswordLoginReachesAnAuthenticatedRoute(t *testing.T) {
	password := `!@#$%^&*()_+-=[]{}|;:',.<>/?~` + " unicode üU0001f510"
	s := testService(t)
	var err error
	if s.passwordHash, err = HashPassword(password); err != nil {
		t.Fatal(err)
	}
	mux := http.NewServeMux()
	s.Mount(mux)
	handler := s.Enforce(mux, Listener{UI: true})

	page := httptest.NewRecorder()
	handler.ServeHTTP(page, secureRequest(http.MethodGet, "/login", nil))
	if page.Code != http.StatusOK {
		t.Fatalf("login page status=%d", page.Code)
	}
	var formToken string
	for _, c := range page.Result().Cookies() {
		if c.Name == loginCookie {
			formToken = c.Value
		}
	}
	if formToken == "" || !strings.Contains(page.Body.String(), `value="`+formToken+`"`) {
		t.Fatal("login page issued no form token")
	}
	form := url.Values{"csrf": {formToken}, "password": {password}}.Encode()
	post := secureRequest(http.MethodPost, "/auth/password", strings.NewReader(form))
	post.Header.Set("Content-Type", "application/x-www-form-urlencoded")
	post.Header.Set("Origin", s.origin)
	post.AddCookie(&http.Cookie{Name: loginCookie, Value: formToken})
	login := httptest.NewRecorder()
	handler.ServeHTTP(login, post)
	if login.Code != http.StatusSeeOther || login.Header().Get("Location") != "/" {
		t.Fatalf("login code=%d location=%q", login.Code, login.Header().Get("Location"))
	}
	var session, csrf, cleared *http.Cookie
	for _, c := range login.Result().Cookies() {
		switch {
		case c.Name == sessionCookie && c.Value != "":
			session = c
		case c.Name == csrfCookie && c.Value != "":
			csrf = c
		case c.Name == loginCookie && c.MaxAge < 0:
			cleared = c
		}
	}
	if session == nil || csrf == nil || cleared == nil || session.Value == csrf.Value {
		t.Fatalf("login cookies: session=%v csrf=%v cleared form token=%v", session, csrf, cleared)
	}

	info := secureRequest(http.MethodGet, "/auth/session", nil)
	info.AddCookie(session)
	info.Header.Set("Origin", s.origin)
	rr := httptest.NewRecorder()
	handler.ServeHTTP(rr, info)
	var got struct {
		Provider, CSRF                 string
		RemainingMs, MaximumLifetimeMs int64
	}
	err = json.Unmarshal(rr.Body.Bytes(), &got, json.MatchCaseInsensitiveNames(true))
	if rr.Code != http.StatusOK || err != nil || got.Provider != "local" || got.CSRF != csrf.Value ||
		got.MaximumLifetimeMs != sessionLifetime.Milliseconds() ||
		sessionLifetime.Milliseconds()-got.RemainingMs > time.Second.Milliseconds() {
		t.Fatalf("session info = %d %s", rr.Code, rr.Body.String())
	}
	measurement := secureRequest(http.MethodPost, "/upload", nil)
	measurement.AddCookie(session)
	measurement.Header.Set("Origin", s.origin)
	measurement.Header.Set("X-CSRF-Token", csrf.Value)
	upload := httptest.NewRecorder()
	s.Enforce(statusHandler(http.StatusNoContent), Listener{UI: true}).ServeHTTP(upload, measurement)
	if upload.Code != http.StatusNoContent {
		t.Fatalf("measurement with the mirrored CSRF token = %d, want 204", upload.Code)
	}
	if len(rr.Result().Cookies()) != 0 || len(upload.Result().Cookies()) != 0 {
		t.Fatal("authenticated activity renewed a cookie; the session lifetime is absolute")
	}
}
