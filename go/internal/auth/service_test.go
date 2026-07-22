package auth

import (
	"bytes"
	"context"
	"crypto/sha256"
	"crypto/tls"
	"encoding/base64"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"net/http/httptest"
	"net/netip"
	"net/url"
	"os"
	"reflect"
	"regexp"
	"strings"
	"testing"
	"time"

	"github.com/coreos/go-oidc/v3/oidc"
	"github.com/zR-JB/graphite-meter/go/internal/config"
	"golang.org/x/oauth2"
)

func testService(t *testing.T) *Service {
	t.Helper()
	h, err := HashPassword("secret")
	if err != nil {
		t.Fatal(err)
	}
	s, err := New(context.Background(), config.AuthConfig{Mode: "password", PublicURL: "https://meter.example", PasswordHash: h, OIDCProviderName: "Authelia"}, nil)
	if err != nil {
		t.Fatal(err)
	}
	return s
}
func secureRequest(method, path string, body *countingReader) *http.Request {
	var r *http.Request
	if body == nil {
		r = httptest.NewRequest(method, "https://meter.example"+path, nil)
	} else {
		r = httptest.NewRequest(method, "https://meter.example"+path, body)
	}
	r.Host = "meter.example"
	r.TLS = &tls.ConnectionState{}
	return r
}

type countingReader struct {
	r *bytes.Reader
	n int
}

func (r *countingReader) Read(p []byte) (int, error) { n, e := r.r.Read(p); r.n += n; return n, e }

func TestOffWrapperIsTransparent(t *testing.T) {
	s, err := New(context.Background(), config.AuthConfig{Mode: "off"}, nil)
	if err != nil {
		t.Fatal(err)
	}
	called := false
	h := s.Wrap(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) { called = true; w.WriteHeader(299) }), Listener{})
	rr := httptest.NewRecorder()
	h.ServeHTTP(rr, httptest.NewRequest("GET", "http://example/anything", nil))
	if !called || rr.Code != 299 {
		t.Fatalf("called=%v code=%d", called, rr.Code)
	}
}
func TestUnauthenticatedRequestRejectedBeforeBodyRead(t *testing.T) {
	s := testService(t)
	body := &countingReader{r: bytes.NewReader(bytes.Repeat([]byte("x"), 1024))}
	called := false
	h := s.Wrap(http.HandlerFunc(func(http.ResponseWriter, *http.Request) { called = true }), Listener{})
	rr := httptest.NewRecorder()
	h.ServeHTTP(rr, secureRequest("POST", "/upload", body))
	if rr.Code != 403 || called || body.n != 0 {
		t.Fatalf("code=%d called=%v bytes=%d", rr.Code, called, body.n)
	}
	if rr.Header().Get("Connection") != "close" {
		t.Fatal("H1 rejection did not close connection")
	}
}

func TestUnauthenticatedUIRootRedirectsButAPIsDoNot(t *testing.T) {
	s := testService(t)
	for _, tc := range []struct {
		name     string
		listener Listener
		path     string
		want     int
	}{
		{"UI root", Listener{UI: true}, "/", http.StatusTemporaryRedirect},
		{"measurement root", Listener{}, "/", http.StatusForbidden},
		{"UI API", Listener{UI: true}, "/preflight", http.StatusForbidden},
	} {
		t.Run(tc.name, func(t *testing.T) {
			h := s.Wrap(http.HandlerFunc(func(http.ResponseWriter, *http.Request) { t.Fatal("called") }), tc.listener)
			rr := httptest.NewRecorder()
			h.ServeHTTP(rr, secureRequest(http.MethodGet, tc.path, nil))
			if rr.Code != tc.want {
				t.Fatalf("code=%d", rr.Code)
			}
			if tc.want == http.StatusTemporaryRedirect && rr.Header().Get("Location") != s.public.String()+"/login" {
				t.Fatalf("location=%q", rr.Header().Get("Location"))
			}
		})
	}
}
func TestSessionRevocationCancelsActiveRequest(t *testing.T) {
	s := testService(t)
	raw, sess, err := s.createSession("subject", "Name", "local", time.Time{})
	if err != nil {
		t.Fatal(err)
	}
	entered := make(chan struct{})
	done := make(chan struct{})
	ended := make(chan bool, 1)
	h := s.Wrap(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		close(entered)
		<-r.Context().Done()
		ended <- SessionEnded(r.Context())
		close(done)
	}), Listener{})
	r := secureRequest("GET", "/download", nil)
	r.AddCookie(&http.Cookie{Name: sessionCookie, Value: raw})
	r.Header.Set("Sec-Fetch-Site", "same-origin")
	go h.ServeHTTP(httptest.NewRecorder(), r)
	<-entered
	s.mu.Lock()
	s.deleteSessionLocked(sess)
	s.mu.Unlock()
	select {
	case <-done:
	case <-time.After(time.Second):
		t.Fatal("request context was not cancelled")
	}
	if !<-ended {
		t.Fatal("session cancellation cause was not preserved")
	}
}

func TestOrdinaryDeadlineIsNotSessionEnd(t *testing.T) {
	ctx, cancel := context.WithTimeout(context.Background(), time.Millisecond)
	defer cancel()
	<-ctx.Done()
	if SessionEnded(ctx) {
		t.Fatal("ordinary operation deadline classified as session expiry")
	}
}
func TestSessionExpiryCancelsAtDeadline(t *testing.T) {
	s := testService(t)
	_, sess, err := s.createSession("expiring", "Name", "local", time.Now().Add(40*time.Millisecond))
	if err != nil {
		t.Fatal(err)
	}
	select {
	case <-sess.ctx.Done():
	case <-time.After(250 * time.Millisecond):
		t.Fatal("session survived its deadline")
	}
}
func TestCookieMutationRequiresOriginAndCSRF(t *testing.T) {
	s := testService(t)
	raw, sess, err := s.createSession("subject", "Name", "local", time.Time{})
	if err != nil {
		t.Fatal(err)
	}
	called := false
	h := s.Wrap(http.HandlerFunc(func(http.ResponseWriter, *http.Request) { called = true }), Listener{})
	for _, tc := range []struct {
		name, origin, csrf string
		want               int
	}{{"missing origin", "", sess.csrf, 403}, {"missing csrf", s.public.String(), "", 403}, {"valid", s.public.String(), sess.csrf, 200}} {
		t.Run(tc.name, func(t *testing.T) {
			called = false
			r := secureRequest("POST", "/upload", nil)
			r.AddCookie(&http.Cookie{Name: sessionCookie, Value: raw})
			r.Header.Set("Origin", tc.origin)
			r.Header.Set("X-CSRF-Token", tc.csrf)
			rr := httptest.NewRecorder()
			h.ServeHTTP(rr, r)
			if rr.Code != tc.want || called != (tc.want == 200) {
				t.Fatalf("code=%d called=%v", rr.Code, called)
			}
		})
	}
}

func TestAuthPagesPreserveSameOriginFormOrigin(t *testing.T) {
	h := http.Header{}
	securityHeaders(h)
	if got := h.Get("Referrer-Policy"); got != "same-origin" {
		t.Fatalf("Referrer-Policy = %q, want same-origin", got)
	}

	h = http.Header{}
	authenticatedSecurityHeaders(h)
	if got := h.Get("Referrer-Policy"); got != "same-origin" {
		t.Fatalf("authenticated Referrer-Policy = %q, want same-origin", got)
	}
}

func TestLoginCSPAllowsOnlyDiscoveredAuthorizationOrigin(t *testing.T) {
	s := testService(t)
	s.oidc = &oidcState{oauth: oauth2.Config{Endpoint: oauth2.Endpoint{AuthURL: "https://login.example:8443/oauth2/authorize"}}}
	h := http.Header{}
	s.loginSecurityHeaders(h)
	policy := h.Get("Content-Security-Policy")
	if !strings.Contains(policy, "form-action 'self' https://login.example:8443;") {
		t.Fatalf("authorization origin missing from CSP: %q", policy)
	}
	if strings.Contains(policy, "/oauth2/authorize") {
		t.Fatalf("authorization path leaked into CSP source: %q", policy)
	}

	s.oidc.oauth.Endpoint.AuthURL = "http://login.example/authorize"
	h = http.Header{}
	s.loginSecurityHeaders(h)
	if strings.Contains(h.Get("Content-Security-Policy"), "login.example") {
		t.Fatalf("clear authorization origin accepted in CSP: %q", h.Get("Content-Security-Policy"))
	}
}

func TestLoginCSRFFailureReasons(t *testing.T) {
	s := testService(t)
	token := "abcdefghijklmnopqrstuvwxyz0123456789"
	request := func(origin, cookie, form string) *http.Request {
		body := url.Values{"csrf": {form}}.Encode()
		r := httptest.NewRequest(http.MethodPost, s.public.String()+"/auth/password", strings.NewReader(body))
		r.Header.Set("Content-Type", "application/x-www-form-urlencoded")
		if origin != "" {
			r.Header.Set("Origin", origin)
		}
		if cookie != "" {
			r.AddCookie(&http.Cookie{Name: loginCookie, Value: cookie})
		}
		if err := r.ParseForm(); err != nil {
			t.Fatal(err)
		}
		return r
	}

	for _, tc := range []struct {
		name, origin, cookie, form, want string
	}{
		{"missing origin", "", token, token, "origin_missing"},
		{"null origin", "null", token, token, "origin_mismatch"},
		{"wrong origin", "https://wrong.example", token, token, "origin_mismatch"},
		{"missing cookie", s.public.String(), "", token, "cookie_missing"},
		{"missing token", s.public.String(), token, "", "token_missing"},
		{"wrong token", s.public.String(), token, "different", "token_mismatch"},
		{"valid", s.public.String(), token, token, ""},
	} {
		t.Run(tc.name, func(t *testing.T) {
			if got := s.csrfFailure(request(tc.origin, tc.cookie, tc.form), "csrf"); got != tc.want {
				t.Fatalf("csrfFailure = %q, want %q", got, tc.want)
			}
		})
	}
}
func TestCookieWebSocketRequiresExactOrigin(t *testing.T) {
	s := testService(t)
	raw, _, _ := s.createSession("subject", "Name", "local", time.Time{})
	h := s.Wrap(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) { w.WriteHeader(204) }), Listener{})
	for _, origin := range []string{"", "https://wrong.example", s.public.String()} {
		r := secureRequest("GET", "/ws/ping", nil)
		r.AddCookie(&http.Cookie{Name: sessionCookie, Value: raw})
		r.Header.Set("Origin", origin)
		rr := httptest.NewRecorder()
		h.ServeHTTP(rr, r)
		want := 403
		if origin == s.public.String() {
			want = 204
		}
		if rr.Code != want {
			t.Errorf("origin %q code=%d", origin, rr.Code)
		}
	}
}
func TestCookieMeasurementAllowsExactOriginFromAlternatePort(t *testing.T) {
	s := testService(t)
	raw, _, _ := s.createSession("subject", "Name", "local", time.Time{})
	h := s.Wrap(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) { w.WriteHeader(204) }), Listener{})
	r := secureRequest("GET", "/download", nil)
	r.Host = "meter.example:7443"
	r.AddCookie(&http.Cookie{Name: sessionCookie, Value: raw})
	r.Header.Set("Origin", s.public.String())
	r.Header.Set("Sec-Fetch-Site", "same-site")
	rr := httptest.NewRecorder()
	h.ServeHTTP(rr, r)
	if rr.Code != 204 {
		t.Fatalf("code=%d", rr.Code)
	}
}
func TestCookieMeasurementRejectsSiblingSiteWithoutExactOrigin(t *testing.T) {
	s := testService(t)
	raw, _, _ := s.createSession("subject", "Name", "local", time.Time{})
	h := s.Wrap(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) { w.WriteHeader(204) }), Listener{})
	for _, origin := range []string{"", "https://evil.example"} {
		r := secureRequest("GET", "/download", nil)
		r.AddCookie(&http.Cookie{Name: sessionCookie, Value: raw})
		r.Header.Set("Sec-Fetch-Site", "same-site")
		if origin != "" {
			r.Header.Set("Origin", origin)
		}
		rr := httptest.NewRecorder()
		h.ServeHTTP(rr, r)
		if rr.Code != http.StatusForbidden {
			t.Fatalf("origin=%q code=%d", origin, rr.Code)
		}
	}
}

func TestCookieMeasurementRequiresPositiveSameOriginEvidence(t *testing.T) {
	s := testService(t)
	raw, _, _ := s.createSession("subject", "Name", "local", time.Time{})
	h := s.Wrap(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) { w.WriteHeader(http.StatusNoContent) }), Listener{})
	for _, site := range []string{"", "none", "cross-site"} {
		r := secureRequest(http.MethodGet, "/probe", nil)
		r.AddCookie(&http.Cookie{Name: sessionCookie, Value: raw})
		if site != "" {
			r.Header.Set("Sec-Fetch-Site", site)
		}
		rr := httptest.NewRecorder()
		h.ServeHTTP(rr, r)
		if rr.Code != http.StatusForbidden {
			t.Fatalf("site=%q code=%d", site, rr.Code)
		}
	}
	r := secureRequest(http.MethodGet, "/probe", nil)
	r.AddCookie(&http.Cookie{Name: sessionCookie, Value: raw})
	r.Header.Set("Sec-Fetch-Site", "same-origin")
	rr := httptest.NewRecorder()
	h.ServeHTTP(rr, r)
	if rr.Code != http.StatusNoContent {
		t.Fatalf("same-origin code=%d", rr.Code)
	}
}

func TestSuccessfulAuthenticatedResponseHasTransportAndFrameHeaders(t *testing.T) {
	s := testService(t)
	raw, _, _ := s.createSession("subject", "Name", "local", time.Time{})
	h := s.Wrap(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) { w.WriteHeader(http.StatusOK) }), Listener{UI: true})
	r := secureRequest(http.MethodGet, "/", nil)
	r.AddCookie(&http.Cookie{Name: sessionCookie, Value: raw})
	rr := httptest.NewRecorder()
	h.ServeHTTP(rr, r)
	if rr.Header().Get("Strict-Transport-Security") == "" || rr.Header().Get("X-Frame-Options") != "DENY" || !strings.Contains(rr.Header().Get("Content-Security-Policy"), "frame-ancestors 'none'") {
		t.Fatalf("headers=%v", rr.Header())
	}
}
func TestBrowserAuthRoutesRequireCanonicalPort(t *testing.T) {
	s := testService(t)
	raw, _, _ := s.createSession("subject", "Name", "local", time.Time{})
	h := s.Wrap(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) { w.WriteHeader(204) }), Listener{UI: true})
	r := secureRequest("GET", "/auth/session", nil)
	r.Host = "meter.example:7443"
	r.AddCookie(&http.Cookie{Name: sessionCookie, Value: raw})
	rr := httptest.NewRecorder()
	h.ServeHTTP(rr, r)
	if rr.Code != http.StatusForbidden {
		t.Fatalf("code=%d", rr.Code)
	}
}
func TestBearerCannotAccessBrowserRoutes(t *testing.T) {
	s := testService(t)
	_, sess, _ := s.createSession("subject", "Name", "local", time.Time{})
	grant, _ := randomToken(32)
	hsh := sha256.Sum256([]byte(grant))
	sess.grants[hsh] = struct{}{}
	s.grants[hsh] = sess
	h := s.Wrap(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) { w.WriteHeader(204) }), Listener{UI: true})
	r := secureRequest("GET", "/auth/session", nil)
	r.Header.Set("Authorization", "Bearer "+grant)
	rr := httptest.NewRecorder()
	h.ServeHTTP(rr, r)
	if rr.Code != 403 {
		t.Fatalf("code=%d", rr.Code)
	}
}
func TestAuthRequiredExposesHeadersCrossOrigin(t *testing.T) {
	s := testService(t)
	h := s.Wrap(http.HandlerFunc(func(http.ResponseWriter, *http.Request) { t.Fatal("called") }), Listener{})
	r := secureRequest("GET", "/download", nil)
	r.Header.Set("Origin", s.public.String())
	rr := httptest.NewRecorder()
	h.ServeHTTP(rr, r)
	if rr.Header().Get("Access-Control-Allow-Origin") != s.public.String() || !strings.Contains(rr.Header().Get("Access-Control-Expose-Headers"), "Graphite-Meter-Auth") {
		t.Fatalf("headers=%v", rr.Header())
	}
}
func TestOffModeReservesAuthRoutes(t *testing.T) {
	s, _ := New(context.Background(), config.AuthConfig{Mode: "off"}, nil)
	mux := http.NewServeMux()
	s.Mount(mux)
	mux.HandleFunc("/", func(w http.ResponseWriter, _ *http.Request) { w.WriteHeader(200) })
	for _, path := range []string{"/login", "/auth/session"} {
		rr := httptest.NewRecorder()
		mux.ServeHTTP(rr, httptest.NewRequest("GET", path, nil))
		if rr.Code != 404 {
			t.Errorf("%s code=%d", path, rr.Code)
		}
	}
}
func TestOIDCTransactionCookieAllowsTopLevelCallback(t *testing.T) {
	rr := httptest.NewRecorder()
	setTransactionCookie(rr, "value", time.Now().Add(time.Minute))
	cookies := rr.Result().Cookies()
	if len(cookies) != 1 || cookies[0].SameSite != http.SameSiteLaxMode {
		t.Fatalf("cookie=%+v", cookies)
	}
}
func TestAttemptLimiterStaysBounded(t *testing.T) {
	s := testService(t)
	now := time.Now()
	for i := 0; i < 2048; i++ {
		s.attempts[fmt.Sprintf("2001:db8::%x", i)] = loginAttempt{times: []time.Time{now}}
	}
	r := secureRequest("POST", "/auth/password", nil)
	r.RemoteAddr = "192.0.2.1:1234"
	if s.allowAttempt(r) {
		t.Fatal("new entry admitted at capacity")
	}
	if len(s.attempts) != 2048 {
		t.Fatalf("attempts=%d", len(s.attempts))
	}
}
func TestPerAddressRejectionsDoNotConsumeGlobalPasswordLimit(t *testing.T) {
	s := testService(t)
	for i := 0; i < 61; i++ {
		r := secureRequest(http.MethodPost, "/auth/password", nil)
		r.RemoteAddr = "192.0.2.1:1234"
		want := i < 5
		if got := s.allowAttempt(r); got != want {
			t.Fatalf("attempt %d allowed=%v want=%v", i+1, got, want)
		}
	}
	if len(s.globalAttempts) != 5 {
		t.Fatalf("global attempts=%d", len(s.globalAttempts))
	}
	r := secureRequest(http.MethodPost, "/auth/password", nil)
	r.RemoteAddr = "198.51.100.1:1234"
	if !s.allowAttempt(r) {
		t.Fatal("locally rejected attempts exhausted the global limit")
	}
}

func TestPasswordLimiterUsesRollingWindow(t *testing.T) {
	s := testService(t)
	now := time.Now()
	s.now = func() time.Time { return now }
	r := secureRequest(http.MethodPost, "/auth/password", nil)
	r.RemoteAddr = "192.0.2.1:1234"
	for i := 0; i < 5; i++ {
		if !s.allowAttempt(r) {
			t.Fatalf("attempt %d rejected", i+1)
		}
	}
	now = now.Add(59 * time.Second)
	if s.allowAttempt(r) {
		t.Fatal("fixed-window boundary admitted a sixth attempt")
	}
	now = now.Add(2 * time.Second)
	if !s.allowAttempt(r) {
		t.Fatal("expired rolling-window attempts were retained")
	}
}
func TestPasswordLimiterHasGlobalCeiling(t *testing.T) {
	s := testService(t)
	for i := 0; i < 60; i++ {
		r := secureRequest(http.MethodPost, "/auth/password", nil)
		r.RemoteAddr = fmt.Sprintf("192.0.2.%d:1234", i+1)
		if !s.allowAttempt(r) {
			t.Fatalf("attempt %d rejected early", i+1)
		}
	}
	r := secureRequest(http.MethodPost, "/auth/password", nil)
	r.RemoteAddr = "198.51.100.1:1234"
	if s.allowAttempt(r) {
		t.Fatal("global password-attempt ceiling was bypassed with a new address")
	}
}

func TestAuthClientAddressUsesOnlyAuthoritativeProxyHeader(t *testing.T) {
	s := testService(t)
	s.trusted = []netip.Prefix{netip.MustParsePrefix("192.0.2.0/24")}
	for _, tc := range []struct {
		name, real, forwarded, xff, want string
		ok                               bool
	}{
		{"authoritative", "198.51.100.8", "", "", "198.51.100.8", true},
		{"forwarded competes", "198.51.100.8", "for=203.0.113.1", "", "", false},
		{"xff competes", "198.51.100.8", "", "203.0.113.1", "", false},
		{"missing authoritative", "", "", "", "", false},
	} {
		t.Run(tc.name, func(t *testing.T) {
			r := secureRequest(http.MethodPost, "/auth/password", nil)
			r.RemoteAddr = "192.0.2.10:443"
			r.Header.Set("X-Real-IP", tc.real)
			r.Header.Set("Forwarded", tc.forwarded)
			r.Header.Set("X-Forwarded-For", tc.xff)
			addr, ok := s.authClientAddress(r)
			if ok != tc.ok || ok && addr.String() != tc.want {
				t.Fatalf("addr=%v ok=%v", addr, ok)
			}
		})
	}
}

func TestMountRegistersOnlyEnabledLoginMethods(t *testing.T) {
	for _, tc := range []struct {
		mode           string
		password, oidc bool
	}{
		{"password", true, false},
		{"oidc", false, true},
		{"hybrid", true, true},
	} {
		t.Run(tc.mode, func(t *testing.T) {
			s := &Service{cfg: config.AuthConfig{Mode: tc.mode}}
			mux := http.NewServeMux()
			s.Mount(mux)
			_, passwordPattern := mux.Handler(httptest.NewRequest(http.MethodPost, "/auth/password", nil))
			_, oidcStartPattern := mux.Handler(httptest.NewRequest(http.MethodPost, "/auth/oidc/start", nil))
			_, oidcCallbackPattern := mux.Handler(httptest.NewRequest(http.MethodGet, "/auth/oidc/callback", nil))
			if (passwordPattern == "POST /auth/password") != tc.password || (oidcStartPattern == "POST /auth/oidc/start") != tc.oidc || (oidcCallbackPattern == "GET /auth/oidc/callback") != tc.oidc {
				t.Fatalf("patterns password=%q oidc-start=%q oidc-callback=%q", passwordPattern, oidcStartPattern, oidcCallbackPattern)
			}
		})
	}
}

func TestLoginRendersOnlyConfiguredMethods(t *testing.T) {
	public, _ := url.Parse("https://meter.example")
	for _, tc := range []struct {
		mode               string
		ready              bool
		password, provider bool
	}{
		{"password", false, true, false},
		{"oidc", false, false, true},
		{"oidc", true, false, true},
		{"hybrid", true, true, true},
	} {
		t.Run(fmt.Sprintf("%s-ready-%v", tc.mode, tc.ready), func(t *testing.T) {
			s := &Service{cfg: config.AuthConfig{Mode: tc.mode, OIDCProviderName: "Provider"}, public: public, loginTemplate: loginTemplate, now: time.Now}
			if tc.provider {
				s.oidc = newOIDCState(s.cfg, "secret")
				if tc.ready {
					s.oidc.provider = &oidc.Provider{}
				}
			}
			rr := httptest.NewRecorder()
			s.login(rr, secureRequest(http.MethodGet, "/login", nil))
			body := rr.Body.String()
			if strings.Contains(body, `action="/auth/password"`) != tc.password || strings.Contains(body, `action="/auth/oidc/start"`) != tc.provider {
				t.Fatalf("unexpected methods in %s", body)
			}
			if tc.provider && strings.Contains(body, " disabled") == tc.ready {
				t.Fatalf("provider ready=%v disabled state mismatch", tc.ready)
			}
			if tc.password && (!strings.Contains(body, `autocomplete="current-password"`) || !strings.Contains(body, `for="password"`)) {
				t.Fatal("password form lacks labeling or password-manager semantics")
			}
		})
	}
}

func TestPasswordLoginPreservesFormEncodedPunctuation(t *testing.T) {
	password := `!@#$%^&*()_+-=[]{}|;:',.<>/?~` + " unicode üU0001f510"
	hash, err := HashPassword(password)
	if err != nil {
		t.Fatal(err)
	}
	s, err := New(context.Background(), config.AuthConfig{Mode: "password", PublicURL: "https://meter.example", PasswordHash: hash, OIDCProviderName: "Authelia"}, nil)
	if err != nil {
		t.Fatal(err)
	}
	mux := http.NewServeMux()
	s.Mount(mux)
	handler := s.Wrap(mux, Listener{UI: true})

	login := httptest.NewRecorder()
	handler.ServeHTTP(login, secureRequest(http.MethodGet, "/login", nil))
	var loginCookieValue string
	for _, cookie := range login.Result().Cookies() {
		if cookie.Name == loginCookie {
			loginCookieValue = cookie.Value
		}
	}
	if loginCookieValue == "" || !strings.Contains(login.Body.String(), `value="`+loginCookieValue+`"`) {
		t.Fatal("login CSRF value missing")
	}

	body := url.Values{"csrf": {loginCookieValue}, "password": {password}}.Encode()
	r := httptest.NewRequest(http.MethodPost, "https://meter.example/auth/password", strings.NewReader(body))
	r.Header.Set("Content-Type", "application/x-www-form-urlencoded")
	r.Header.Set("Origin", "https://meter.example")
	r.AddCookie(&http.Cookie{Name: loginCookie, Value: loginCookieValue})
	result := httptest.NewRecorder()
	handler.ServeHTTP(result, r)
	if result.Code != http.StatusSeeOther || result.Header().Get("Location") != "/" {
		t.Fatalf("code=%d location=%q", result.Code, result.Header().Get("Location"))
	}
	foundSession := false
	for _, cookie := range result.Result().Cookies() {
		foundSession = foundSession || cookie.Name == sessionCookie && cookie.Value != ""
	}
	if !foundSession {
		t.Fatal("successful password login did not create a session")
	}
}
func TestPerSubjectSessionLimitRevokesOldest(t *testing.T) {
	s := testService(t)
	var oldest *session
	for i := 0; i < maxSubjectSessions+1; i++ {
		_, sess, err := s.createSession("same", "Name", "local", time.Time{})
		if err != nil {
			t.Fatal(err)
		}
		if i == 0 {
			oldest = sess
		}
		s.now = func() time.Time { return time.Now().Add(time.Duration(i+1) * time.Millisecond) }
	}
	select {
	case <-oldest.ctx.Done():
	default:
		t.Fatal("oldest session was not revoked")
	}
	if len(s.sessions) != maxSubjectSessions {
		t.Fatalf("sessions=%d", len(s.sessions))
	}
}
func TestUnknownCLIChallengeAllocatesNothing(t *testing.T) {
	s := testService(t)
	verifier := "not-known"
	body := `{"verifier":"` + verifier + `"}`
	rr := httptest.NewRecorder()
	r := secureRequest("POST", "/auth/cli/token", nil)
	r.Body = http.NoBody
	r.Body = io.NopCloser(strings.NewReader(body))
	s.cliToken(rr, r)
	if rr.Code != http.StatusAccepted || len(s.approvals) != 0 {
		t.Fatalf("code=%d approvals=%d", rr.Code, len(s.approvals))
	}
	sum := sha256.Sum256([]byte(verifier))
	if _, ok := s.approvals[base64.RawURLEncoding.EncodeToString(sum[:])]; ok {
		t.Fatal("unknown verifier allocated state")
	}
}
func TestVerificationCodeIsAlwaysEightCharacters(t *testing.T) {
	for i := 0; i < 100; i++ {
		challenge, err := randomToken(32)
		if err != nil {
			t.Fatal(err)
		}
		if code := verificationCode(challenge); len(code) != 8 {
			t.Fatalf("code=%q length=%d", code, len(code))
		}
	}
}

func TestLoginFailurePreservesValidCLIChallenge(t *testing.T) {
	s := testService(t)
	verifier := "terminal-verifier"
	sum := sha256.Sum256([]byte(verifier))
	challenge := base64.RawURLEncoding.EncodeToString(sum[:])
	r := secureRequest(http.MethodPost, "/auth/password", nil)
	r.Form = url.Values{"challenge": {challenge}}
	rr := httptest.NewRecorder()
	s.loginFailure(rr, r)
	location, err := url.Parse(rr.Header().Get("Location"))
	if err != nil || location.Query().Get("challenge") != challenge || location.Query().Get("error") != "1" {
		t.Fatalf("location=%q err=%v", rr.Header().Get("Location"), err)
	}
	r.Form.Set("challenge", "invalid")
	rr = httptest.NewRecorder()
	s.loginFailure(rr, r)
	location, _ = url.Parse(rr.Header().Get("Location"))
	if location.Query().Has("challenge") {
		t.Fatal("invalid challenge was reflected")
	}
}
func TestCLIApprovalExchangeIsSingleUseAndRevokedWithSession(t *testing.T) {
	s := testService(t)
	_, sess, _ := s.createSession("subject", "Name", "local", time.Time{})
	verifier := "terminal-verifier"
	sum := sha256.Sum256([]byte(verifier))
	challenge := base64.RawURLEncoding.EncodeToString(sum[:])
	s.approvals[challenge] = &cliApproval{challenge: challenge, session: sess, expires: time.Now().Add(time.Minute), approved: true}
	exchange := func() *httptest.ResponseRecorder {
		rr := httptest.NewRecorder()
		r := secureRequest("POST", "/auth/cli/token", nil)
		r.Body = io.NopCloser(strings.NewReader(`{"verifier":"` + verifier + `"}`))
		s.cliToken(rr, r)
		return rr
	}
	first := exchange()
	if first.Code != 200 {
		t.Fatalf("first code=%d body=%s", first.Code, first.Body.String())
	}
	var out struct {
		Token string `json:"token"`
	}
	if err := json.Unmarshal(first.Body.Bytes(), &out); err != nil || out.Token == "" {
		t.Fatalf("token response: %v %q", err, first.Body.String())
	}
	if _, ok := s.authenticateGrant(out.Token); !ok {
		t.Fatal("grant not accepted")
	}
	if second := exchange(); second.Code != http.StatusAccepted {
		t.Fatalf("replay code=%d", second.Code)
	}
	s.mu.Lock()
	s.deleteSessionLocked(sess)
	s.mu.Unlock()
	if _, ok := s.authenticateGrant(out.Token); ok {
		t.Fatal("grant survived parent logout")
	}
}
func TestCLIGrantSetIsBounded(t *testing.T) {
	s := testService(t)
	_, sess, _ := s.createSession("subject", "Name", "local", time.Time{})
	for i := 0; i < 20; i++ {
		verifier := fmt.Sprintf("verifier-%d", i)
		sum := sha256.Sum256([]byte(verifier))
		challenge := base64.RawURLEncoding.EncodeToString(sum[:])
		s.approvals[challenge] = &cliApproval{challenge: challenge, session: sess, expires: time.Now().Add(time.Minute), approved: true}
		rr := httptest.NewRecorder()
		r := secureRequest("POST", "/auth/cli/token", nil)
		r.Body = io.NopCloser(strings.NewReader(`{"verifier":"` + verifier + `"}`))
		s.cliToken(rr, r)
		if rr.Code != 200 {
			t.Fatalf("exchange %d code=%d", i, rr.Code)
		}
	}
	if len(sess.grants) > 8 {
		t.Fatalf("grants=%d", len(sess.grants))
	}
}
func TestLoginPaletteMatchesApplicationTokens(t *testing.T) {
	css, err := os.ReadFile("../../../client/src/app.css")
	if err != nil {
		t.Fatal(err)
	}
	values := func(source, name string) []string {
		re := regexp.MustCompile(`--` + regexp.QuoteMeta(name) + `:\s*([^;]+);`)
		matches := re.FindAllStringSubmatch(source, -1)
		out := make([]string, 0, len(matches))
		for _, match := range matches {
			out = append(out, strings.TrimSpace(match[1]))
		}
		return out
	}
	for authName, appName := range map[string]string{"canvas": "canvas", "surface": "surface-1", "border": "border", "text": "text", "muted": "text-muted", "brand": "brand", "error": "err"} {
		if authValues, appValues := values(authCSS, authName), values(string(css), appName); !reflect.DeepEqual(authValues, appValues) {
			t.Errorf("token %s values %v do not match application token %s values %v", authName, authValues, appName, appValues)
		}
	}
}

func TestGeneratedAuthAssetsAreCurrent(t *testing.T) {
	for path, generated := range map[string]string{
		"../../../client/src/auth/auth.css":      authCSS,
		"../../../client/src/auth/login.tmpl":    loginHTML,
		"../../../client/src/auth/cli.tmpl":      cliHTML,
		"../../../client/src/auth/cli-done.tmpl": cliDoneHTML,
	} {
		contents, err := os.ReadFile(path)
		if err != nil {
			t.Fatal(err)
		}
		if string(contents) != generated {
			t.Fatalf("%s changed without running go generate ./internal/auth", path)
		}
	}
}
