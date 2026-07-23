package auth

import (
	"context"
	"net/http"
	"net/http/httptest"
	"net/netip"
	"net/url"
	"strings"
	"testing"
	"time"

	"github.com/zR-JB/graphite-meter/go/internal/config"
)

// proxiedService trusts 192.0.2.0/24, the documented reverse-proxy topology.
func proxiedService(t *testing.T) *Service {
	t.Helper()
	hash, err := HashPassword("secret")
	if err != nil {
		t.Fatal(err)
	}
	s, err := New(context.Background(), config.AuthConfig{Mode: "password", PublicURL: "https://meter.example", PasswordHash: hash, OIDCProviderName: "Authelia"}, []netip.Prefix{netip.MustParsePrefix("192.0.2.0/24")}, false)
	if err != nil {
		t.Fatal(err)
	}
	return s
}

// clearRequest is a cleartext request as it arrives on the H1 listener: no
// TLS, so everything the boundary believes about it comes from the peer
// address and the forwarded headers.
func clearRequest(method, path, remote string) *http.Request {
	r := httptest.NewRequest(method, "http://meter.example"+path, nil)
	r.Host = "meter.example"
	r.RemoteAddr = remote
	return r
}

// REVERSE_PROXY.md is built on exactly this boundary: forwarded headers are
// evidence only from a peer inside GM_TRUSTED_PROXIES. An outsider that
// reaches the cleartext listener directly and claims the proxy's headers must
// gain nothing.
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
			name:    "trusted peer sending a duplicated proto header",
			remote:  "192.0.2.10:40000",
			headers: map[string][]string{"X-Forwarded-Proto": {"https", "https"}, "X-Forwarded-Host": {"meter.example"}},
		},
		{
			name:    "trusted peer sending a comma-joined proto header",
			remote:  "192.0.2.10:40000",
			headers: map[string][]string{"X-Forwarded-Proto": {"https,http"}, "X-Forwarded-Host": {"meter.example"}},
		},
		{
			name:    "trusted peer sending a comma-joined host header",
			remote:  "192.0.2.10:40000",
			headers: map[string][]string{"X-Forwarded-Proto": {"https"}, "X-Forwarded-Host": {"meter.example,evil.example"}},
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

			// The same request through the whole boundary: an untrusted claim
			// must not reach the login surface either.
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

// A trusted proxy that forwards a client address must be the only source of
// one. Ambiguous or absent evidence has to fail closed, or an attempt budget
// can be charged to an address the attacker chose.
func TestAuthClientAddressFailsClosedBehindATrustedProxy(t *testing.T) {
	s := proxiedService(t)
	for _, tc := range []struct {
		name    string
		remote  string
		headers map[string][]string
		want    string
	}{
		{"direct peer is itself", "198.51.100.9:40000", nil, "198.51.100.9"},
		{"direct peer keeps its own headers out of it", "198.51.100.9:40000", map[string][]string{"X-Real-IP": {"203.0.113.1"}}, "198.51.100.9"},
		{"proxied peer without X-Real-IP", "192.0.2.10:40000", nil, ""},
		{"proxied peer with X-Forwarded-For present", "192.0.2.10:40000", map[string][]string{"X-Real-IP": {"203.0.113.1"}, "X-Forwarded-For": {"203.0.113.1"}}, ""},
		{"proxied peer with Forwarded present", "192.0.2.10:40000", map[string][]string{"X-Real-IP": {"203.0.113.1"}, "Forwarded": {"for=203.0.113.1"}}, ""},
		{"proxied peer with a duplicated X-Real-IP", "192.0.2.10:40000", map[string][]string{"X-Real-IP": {"203.0.113.1", "203.0.113.2"}}, ""},
		{"proxied peer with a comma-joined X-Real-IP", "192.0.2.10:40000", map[string][]string{"X-Real-IP": {"203.0.113.1,203.0.113.2"}}, ""},
		{"proxied peer with a single X-Real-IP", "192.0.2.10:40000", map[string][]string{"X-Real-IP": {"203.0.113.1"}}, "203.0.113.1"},
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

// The cookie names carry the __Host- prefix, which browsers enforce only when
// the attributes match. A regression in the setters would otherwise pass every
// other test in this package while silently dropping the prefix's guarantees.
func TestCookieAttributesSatisfyTheHostPrefix(t *testing.T) {
	s := testService(t)
	_, sess, err := s.createSession("local-operator", "Local operator", "local", time.Time{})
	if err != nil {
		t.Fatal(err)
	}
	rr := httptest.NewRecorder()
	setSessionCookie(rr, sessionCookie, "value-value-value-value", sess.expires)
	setCSRFCookie(rr, sess.csrf, sess.expires)
	setSessionCookie(rr, loginCookie, "value-value-value-value", sess.expires)
	setTransactionCookie(rr, "value-value-value-value", sess.expires)

	want := map[string]struct {
		httpOnly bool
		sameSite http.SameSite
	}{
		sessionCookie: {httpOnly: true, sameSite: http.SameSiteStrictMode},
		loginCookie:   {httpOnly: true, sameSite: http.SameSiteStrictMode},
		// The SPA mirrors this one into X-CSRF-Token, so it is readable by
		// design; it carries no authority on its own.
		csrfCookie: {httpOnly: false, sameSite: http.SameSiteStrictMode},
		// The OIDC callback is a cross-site navigation, so this one must be
		// Lax or the transaction cannot be matched on return.
		transactionCookie: {httpOnly: true, sameSite: http.SameSiteLaxMode},
	}
	seen := map[string]bool{}
	for _, c := range rr.Result().Cookies() {
		expect, ok := want[c.Name]
		if !ok {
			t.Fatalf("unexpected cookie %q", c.Name)
		}
		seen[c.Name] = true
		if !strings.HasPrefix(c.Name, "__Host-") {
			t.Fatalf("%s does not carry the __Host- prefix", c.Name)
		}
		if !c.Secure {
			t.Fatalf("%s is not Secure; __Host- requires it", c.Name)
		}
		if c.Path != "/" {
			t.Fatalf("%s has Path=%q; __Host- requires /", c.Name, c.Path)
		}
		if c.Domain != "" {
			t.Fatalf("%s has Domain=%q; __Host- forbids it", c.Name, c.Domain)
		}
		if c.HttpOnly != expect.httpOnly {
			t.Fatalf("%s HttpOnly=%t, want %t", c.Name, c.HttpOnly, expect.httpOnly)
		}
		if c.SameSite != expect.sameSite {
			t.Fatalf("%s SameSite=%v, want %v", c.Name, c.SameSite, expect.sameSite)
		}
	}
	for name := range want {
		if !seen[name] {
			t.Fatalf("%s was never set", name)
		}
	}
}

func TestClearedCookiesKeepTheHostPrefixAttributes(t *testing.T) {
	rr := httptest.NewRecorder()
	clearCookie(rr, sessionCookie)
	clearTransactionCookie(rr)
	for _, c := range rr.Result().Cookies() {
		if !c.Secure || c.Path != "/" || c.Domain != "" || c.MaxAge >= 0 {
			t.Fatalf("cleared cookie %q = %+v", c.Name, c)
		}
	}
}

// The whole password path, end to end through Mount and Enforce: the login
// page issues a form token, the form exchanges it for a session, and the
// session then reaches an authenticated route. Any one of those breaking is
// invisible to the negative tests, which all assert 403.
func TestPasswordLoginReachesAnAuthenticatedRoute(t *testing.T) {
	s := testService(t)
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
	if formToken == "" {
		t.Fatal("login page issued no form token")
	}

	form := url.Values{"csrf": {formToken}, "password": {"secret"}}.Encode()
	post := httptest.NewRequest(http.MethodPost, "https://meter.example/auth/password", strings.NewReader(form))
	post.Host = "meter.example"
	post.TLS = secureRequest(http.MethodGet, "/", nil).TLS
	post.Header.Set("Content-Type", "application/x-www-form-urlencoded")
	post.Header.Set("Origin", s.public.String())
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
	if session == nil || csrf == nil {
		t.Fatalf("login did not set both cookies: session=%v csrf=%v", session, csrf)
	}
	if cleared == nil {
		t.Fatal("the single-use form token was not cleared")
	}
	if session.Value == csrf.Value {
		t.Fatal("session token and CSRF token are the same value")
	}

	info := secureRequest(http.MethodGet, "/auth/session", nil)
	info.AddCookie(session)
	info.Header.Set("Origin", s.public.String())
	rr := httptest.NewRecorder()
	handler.ServeHTTP(rr, info)
	if rr.Code != http.StatusOK {
		t.Fatalf("authenticated route status=%d, want 200", rr.Code)
	}
	if !strings.Contains(rr.Body.String(), `"provider":"local"`) || !strings.Contains(rr.Body.String(), csrf.Value) {
		t.Fatalf("session info = %s", rr.Body.String())
	}

	// And a measurement POST with the mirrored CSRF header must pass, while
	// the same request without it must not.
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
			measurement.Header.Set("Origin", s.public.String())
			if tc.header != "" {
				measurement.Header.Set("X-CSRF-Token", tc.header)
			}
			rr := httptest.NewRecorder()
			s.Enforce(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
				w.WriteHeader(http.StatusNoContent)
			}), Listener{UI: true}).ServeHTTP(rr, measurement)
			if rr.Code != tc.want {
				t.Fatalf("status=%d, want %d", rr.Code, tc.want)
			}
		})
	}
}
