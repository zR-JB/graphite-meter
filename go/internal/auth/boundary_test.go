package auth

import (
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
	for _, tc := range []struct {
		name           string
		remote         string
		headers        map[string][]string
		wantSecure     bool
		wantCanonical  bool
		wantAuthorized bool
	}{
		{
			name:    "untrusted peer forging both headers",
			remote:  "198.51.100.9:40000",
			headers: map[string][]string{"X-Forwarded-Proto": {"https"}, "X-Forwarded-Host": {"meter.example"}},
		},
		{
			name:   "trusted peer sending no headers",
			remote: "192.0.2.10:40000",
		},
		{
			name:    "trusted peer sending proto only",
			remote:  "192.0.2.10:40000",
			headers: map[string][]string{"X-Forwarded-Proto": {"https"}},
		},
		{
			name:   "trusted peer sending a duplicated proto header",
			remote: "192.0.2.10:40000",
			headers: map[string][]string{"X-Forwarded-Proto": {"https", "https"},
				"X-Forwarded-Host": {"meter.example"}},
		},
		{
			name:    "trusted peer sending a comma-joined proto header",
			remote:  "192.0.2.10:40000",
			headers: map[string][]string{"X-Forwarded-Proto": {"https,http"}, "X-Forwarded-Host": {"meter.example"}},
		},
		{
			name:   "trusted peer sending a comma-joined host header",
			remote: "192.0.2.10:40000",
			headers: map[string][]string{
				"X-Forwarded-Proto": {"https"}, "X-Forwarded-Host": {"meter.example,evil.example"},
			},
		},
		{
			name:    "trusted peer sending a foreign host",
			remote:  "192.0.2.10:40000",
			headers: map[string][]string{"X-Forwarded-Proto": {"https"}, "X-Forwarded-Host": {"evil.example"}},
		},
		{
			name:    "trusted peer sending http",
			remote:  "192.0.2.10:40000",
			headers: map[string][]string{"X-Forwarded-Proto": {"http"}, "X-Forwarded-Host": {"meter.example"}},
		},
		{
			name:           "trusted peer with the documented header pair",
			remote:         "192.0.2.10:40000",
			headers:        map[string][]string{"X-Forwarded-Proto": {"https"}, "X-Forwarded-Host": {"meter.example"}},
			wantSecure:     true,
			wantCanonical:  true,
			wantAuthorized: true,
		},
	} {
		t.Run(tc.name, func(t *testing.T) {
			r := clearRequest(http.MethodGet, "/login", tc.remote)
			for name, values := range tc.headers {
				for _, v := range values {
					r.Header.Add(name, v)
				}
			}
			got := s.requestTrust(r)
			if got.Secure != tc.wantSecure || got.Canonical != tc.wantCanonical {
				t.Fatalf("requestTrust = %+v, want {Secure:%t Canonical:%t}", got, tc.wantSecure, tc.wantCanonical)
			}

			mux := http.NewServeMux()
			s.Mount(mux)
			rr := httptest.NewRecorder()
			s.Enforce(mux, Listener{UI: true}).ServeHTTP(rr, r)
			reached := rr.Code == http.StatusOK
			if reached != tc.wantAuthorized {
				t.Fatalf("login page status = %d, reachable=%t, want reachable=%t", rr.Code, reached, tc.wantAuthorized)
			}
		})
	}
}

func TestAuthClientAddressFailsClosedBehindATrustedProxy(t *testing.T) {
	s := proxiedService(t)
	for _, tc := range []struct {
		name    string
		remote  string
		headers map[string][]string
		want    string
	}{
		{"direct peer is itself", "198.51.100.9:40000", nil, "198.51.100.9"},
		{"direct peer keeps its own headers out of it", "198.51.100.9:40000",
			map[string][]string{"X-Real-IP": {"203.0.113.1"}}, "198.51.100.9"},
		{"proxied peer without X-Real-IP", "192.0.2.10:40000", nil, ""},
		{"proxied peer with X-Forwarded-For present", "192.0.2.10:40000",
			map[string][]string{"X-Real-IP": {"203.0.113.1"}, "X-Forwarded-For": {"203.0.113.1"}}, ""},
		{"proxied peer with Forwarded present", "192.0.2.10:40000",
			map[string][]string{"X-Real-IP": {"203.0.113.1"}, "Forwarded": {"for=203.0.113.1"}}, ""},
		{"proxied peer with a duplicated X-Real-IP", "192.0.2.10:40000",
			map[string][]string{"X-Real-IP": {"203.0.113.1", "203.0.113.2"}}, ""},
		{"proxied peer with a comma-joined X-Real-IP", "192.0.2.10:40000",
			map[string][]string{"X-Real-IP": {"203.0.113.1,203.0.113.2"}}, ""},
		{"proxied peer with a single X-Real-IP", "192.0.2.10:40000",
			map[string][]string{"X-Real-IP": {"203.0.113.1"}}, "203.0.113.1"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			r := clearRequest(http.MethodPost, "/auth/password", tc.remote)
			for name, values := range tc.headers {
				for _, v := range values {
					r.Header.Add(name, v)
				}
			}
			addr, ok := s.authClientAddress(r)
			if tc.want == "" {
				if ok {
					t.Fatalf("resolved a client address (%s) from ambiguous evidence", addr)
				}
				return
			}
			if !ok || addr.String() != tc.want {
				t.Fatalf("authClientAddress = (%s, %t), want %s", addr, ok, tc.want)
			}
		})
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
	if rr.Code != http.StatusOK || !strings.Contains(rr.Body.String(), `"provider":"local"`) ||
		!strings.Contains(rr.Body.String(), csrf.Value) {
		t.Fatalf("session info = %d %s", rr.Code, rr.Body.String())
	}
	for _, tc := range []struct {
		name   string
		header string
		want   int
	}{
		{"with the mirrored CSRF token", csrf.Value, http.StatusNoContent},
		{"without it", "", http.StatusForbidden},
		{"with the session token instead", session.Value, http.StatusForbidden},
	} {
		t.Run(tc.name, func(t *testing.T) {
			measurement := secureRequest(http.MethodPost, "/upload", nil)
			measurement.AddCookie(session)
			measurement.Header.Set("Origin", s.origin)
			if tc.header != "" {
				measurement.Header.Set("X-CSRF-Token", tc.header)
			}
			rr := httptest.NewRecorder()
			s.Enforce(statusHandler(http.StatusNoContent), Listener{UI: true}).ServeHTTP(rr, measurement)
			if rr.Code != tc.want {
				t.Fatalf("status=%d, want %d", rr.Code, tc.want)
			}
		})
	}
}
