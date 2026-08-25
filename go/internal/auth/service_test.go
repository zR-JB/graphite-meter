package auth

import (
	"bytes"
	"context"
	"crypto/sha256"
	"crypto/tls"
	"encoding/base64"
	"encoding/json/v2"
	"fmt"
	"html/template"
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
	s, err := New(t.Context(), config.AuthConfig{Mode: "password", PublicURL: "https://meter.example", PasswordHash: h, OIDCProviderName: "Authelia"}, nil, false)
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
	s, err := New(t.Context(), config.AuthConfig{Mode: "off"}, nil, false)
	if err != nil {
		t.Fatal(err)
	}
	called := false
	h := s.Enforce(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) { called = true; w.WriteHeader(299) }), Listener{})
	rr := httptest.NewRecorder()
	h.ServeHTTP(rr, httptest.NewRequest("GET", "http://example/anything", nil))
	if !called || rr.Code != 299 {
		t.Fatalf("called=%v code=%d, want true and 299", called, rr.Code)
	}
}
func TestUnauthenticatedRequestRejectedBeforeBodyRead(t *testing.T) {
	s := testService(t)
	body := &countingReader{r: bytes.NewReader(bytes.Repeat([]byte("x"), 1024))}
	called := false
	h := s.Enforce(http.HandlerFunc(func(http.ResponseWriter, *http.Request) { called = true }), Listener{})
	rr := httptest.NewRecorder()
	h.ServeHTTP(rr, secureRequest("POST", "/upload", body))
	if rr.Code != 403 || called || body.n != 0 {
		t.Fatalf("code=%d called=%v bytes=%d, want 403, false and 0", rr.Code, called, body.n)
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
			h := s.Enforce(http.HandlerFunc(func(http.ResponseWriter, *http.Request) { t.Fatal("called") }), tc.listener)
			rr := httptest.NewRecorder()
			h.ServeHTTP(rr, secureRequest(http.MethodGet, tc.path, nil))
			if rr.Code != tc.want {
				t.Fatalf("code=%d, want %d", rr.Code, tc.want)
			}
			if want := s.public.String() + "/login"; tc.want == http.StatusTemporaryRedirect && rr.Header().Get("Location") != want {
				t.Fatalf("location=%q, want %q", rr.Header().Get("Location"), want)
			}
		})
	}
}
func TestSessionRevocationCancelsActiveRequest(t *testing.T) {
	s := testService(t)
	raw, sess, err := s.createSession("subject", "Name", "local")
	if err != nil {
		t.Fatal(err)
	}
	entered := make(chan struct{})
	done := make(chan struct{})
	ended := make(chan bool, 1)
	h := s.Enforce(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
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
	ctx, cancel := context.WithTimeout(t.Context(), time.Millisecond)
	defer cancel()
	<-ctx.Done()
	if SessionEnded(ctx) {
		t.Fatal("ordinary operation deadline classified as session expiry")
	}
}
func TestSessionExpiryCancelsAtDeadline(t *testing.T) {
	s := testService(t)
	_, sess, err := s.createSessionUntil("expiring", "Name", "local", time.Now().Add(40*time.Millisecond))
	if err != nil {
		t.Fatal(err)
	}
	select {
	case <-sess.ctx.Done():
	case <-time.After(250 * time.Millisecond):
		t.Fatal("session survived its deadline")
	}
}

func TestSessionUsesAbsolutePolicy(t *testing.T) {
	s := testService(t)
	_, sess, err := s.createSession("subject", "Name", "local")
	if err != nil {
		t.Fatal(err)
	}
	if lifetime := sess.expires.Sub(sess.created); lifetime != sessionLifetime {
		t.Fatalf("session lifetime=%v, want %v", lifetime, sessionLifetime)
	}
}

func TestSessionInfoReportsServerCalculatedLifetime(t *testing.T) {
	s := testService(t)
	_, sess, err := s.createSession("subject", "Name", "local")
	if err != nil {
		t.Fatal(err)
	}
	expires := sess.expires
	r := secureRequest(http.MethodGet, "/auth/session", nil)
	p := Principal{Subject: sess.subject, Name: sess.name, Provider: sess.provider, Expires: sess.expires, session: sess}
	r = r.WithContext(context.WithValue(r.Context(), principalKey{}, p))
	rr := httptest.NewRecorder()
	s.sessionInfo(rr, r)
	var got struct {
		RemainingMs       int64 `json:"remainingMs"`
		MaximumLifetimeMs int64 `json:"maximumLifetimeMs"`
	}
	if err := json.Unmarshal(rr.Body.Bytes(), &got); err != nil {
		t.Fatal(err)
	}
	if got.MaximumLifetimeMs != sessionLifetime.Milliseconds() {
		t.Fatalf("maximumLifetimeMs=%d, want %d", got.MaximumLifetimeMs, sessionLifetime.Milliseconds())
	}
	if got.RemainingMs < (sessionLifetime-time.Second).Milliseconds() || got.RemainingMs > sessionLifetime.Milliseconds() {
		t.Fatalf("remainingMs=%d, want a fresh eight-hour session", got.RemainingMs)
	}
	if len(rr.Result().Cookies()) != 0 {
		t.Fatal("session info renewed a cookie")
	}
	if !sess.expires.Equal(expires) {
		t.Fatal("session info changed the absolute expiry")
	}
}

func TestAuthenticatedActivityDoesNotExtendSession(t *testing.T) {
	s := testService(t)
	raw, sess, err := s.createSession("subject", "Name", "local")
	if err != nil {
		t.Fatal(err)
	}
	expires := sess.expires
	for _, path := range []string{"/", "/download"} {
		r := secureRequest(http.MethodGet, path, nil)
		r.AddCookie(&http.Cookie{Name: sessionCookie, Value: raw})
		r.Header.Set("Sec-Fetch-Site", "same-origin")
		rr := httptest.NewRecorder()
		s.Enforce(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
			w.WriteHeader(http.StatusNoContent)
		}), Listener{UI: true}).ServeHTTP(rr, r)
		if rr.Code != http.StatusNoContent || len(rr.Result().Cookies()) != 0 {
			t.Fatalf("path=%s status=%d cookies=%d", path, rr.Code, len(rr.Result().Cookies()))
		}
	}
	if _, ok := s.consumeWebTransportToken(mintForSession(t, s, sess)); !ok {
		t.Fatal("fresh reconnect token was refused")
	}
	if !sess.expires.Equal(expires) {
		t.Fatal("authenticated activity changed the absolute expiry")
	}
}

func TestCookieMutationRequiresOriginAndCSRF(t *testing.T) {
	s := testService(t)
	raw, sess, err := s.createSession("subject", "Name", "local")
	if err != nil {
		t.Fatal(err)
	}
	called := false
	h := s.Enforce(http.HandlerFunc(func(http.ResponseWriter, *http.Request) { called = true }), Listener{})
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
				t.Fatalf("code=%d called=%v, want code %d and called=%v", rr.Code, called, tc.want, tc.want == 200)
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
	testService(t).authenticatedSecurityHeaders(h)
	if got := h.Get("Referrer-Policy"); got != "same-origin" {
		t.Fatalf("authenticated Referrer-Policy = %q, want same-origin", got)
	}
}

func TestAppCSPPinsScriptsAndConnectSrc(t *testing.T) {
	// The baseline directives and connect-src 'self' are always present;
	// script-src appears only when a real client build supplies a hash.
	base := appCSP("", "")
	for _, want := range []string{
		"frame-ancestors 'none'", "base-uri 'none'", "object-src 'none'",
		"form-action 'self'", "connect-src 'self'",
	} {
		if !strings.Contains(base, want) {
			t.Fatalf("appCSP missing %q: %s", want, base)
		}
	}
	if strings.Contains(base, "script-src") {
		t.Fatalf("appCSP pinned script-src without a build: %s", base)
	}

	pinned := appCSP("ABC123", "https://probe.example https://ping.example")
	if !strings.Contains(pinned, "script-src 'self' 'sha256-ABC123'") {
		t.Fatalf("appCSP with a hash did not pin script-src: %s", pinned)
	}
	// The advertised cross-origin targets extend connect-src; nothing else may.
	if !strings.Contains(pinned, "connect-src 'self' https://probe.example https://ping.example") {
		t.Fatalf("appCSP did not admit the advertised measurement origins: %s", pinned)
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

// The pre-paint theme script is only allowed to run because the CSP pins its
// digest, so every page must ship the exact bytes that digest covers.
func TestAuthPagesCarryTheScriptPinnedByCSP(t *testing.T) {
	digest := func(asset string) string {
		sum := sha256.Sum256([]byte(asset))
		return "'sha256-" + base64.StdEncoding.EncodeToString(sum[:]) + "'"
	}
	pin := "script-src " + digest(authThemeJS) + " " + digest(authPendingJS)
	if policy := authPageCSP(""); !strings.Contains(policy, pin) {
		t.Fatalf("CSP %q does not pin both embedded scripts", policy)
	}
	// pending.js posts the sign-in forms with fetch; without connect-src 'self'
	// the login CSP's default-src 'none' blocks it and sign-in silently fails.
	if policy := authPageCSP(""); !strings.Contains(policy, "connect-src 'self'") {
		t.Fatalf("login CSP %q does not allow the same-origin sign-in fetch", policy)
	}

	pages := map[string]struct {
		tmpl *template.Template
		data any
	}{
		"login":    {loginTemplate, loginView{Styles: authStyles, Password: true, OIDC: true, Provider: "Provider"}},
		"cli":      {cliTemplate, map[string]any{"Styles": authStyles, "Code": "ABCD-1234", "Challenge": "c", "CSRF": "t"}},
		"cli-done": {cliDoneTemplate, map[string]any{"Styles": authStyles}},
		"continue": {continueTemplate, map[string]any{"Styles": authStyles, "Challenge": "c"}},
	}
	for name, page := range pages {
		var rendered bytes.Buffer
		if err := page.tmpl.Execute(&rendered, page.data); err != nil {
			t.Fatalf("%s: %v", name, err)
		}
		// A rendered script must match a hashed asset byte for byte or the CSP
		// blocks it; html/template's JS lexer strips comments from literal text.
		rest := rendered.String()
		scripts := 0
		for {
			_, after, found := strings.Cut(rest, "<script>")
			if !found {
				break
			}
			script, remainder, closed := strings.Cut(after, "</script>")
			if !closed {
				t.Fatalf("%s renders an unterminated inline script", name)
			}
			if script != authThemeJS && script != authPendingJS {
				t.Fatalf("%s script does not match a digest pinned in the CSP", name)
			}
			scripts++
			rest = remainder
		}
		if scripts == 0 {
			t.Fatalf("%s renders no inline script", name)
		}
	}
}

func TestFormPagesCarryTheSubmitScript(t *testing.T) {
	pages := map[string]struct {
		tmpl *template.Template
		data any
	}{
		"login": {loginTemplate, loginView{Styles: authStyles, Password: true, OIDC: true, Provider: "Provider"}},
		"cli":   {cliTemplate, map[string]any{"Styles": authStyles, "Code": "ABCD-1234", "Challenge": "c", "CSRF": "t"}},
	}
	for name, page := range pages {
		var rendered bytes.Buffer
		if err := page.tmpl.Execute(&rendered, page.data); err != nil {
			t.Fatalf("%s: %v", name, err)
		}
		if !strings.Contains(rendered.String(), authPendingJS) {
			t.Errorf("%s submits a form without the pending-state script", name)
		}
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
		name, origin, cookie, form string
		want                       reason
		wantOK                     bool
	}{
		{"missing origin", "", token, token, reasonCSRFOriginMissing, false},
		{"null origin", "null", token, token, reasonCSRFOriginMismatch, false},
		{"wrong origin", "https://wrong.example", token, token, reasonCSRFOriginMismatch, false},
		{"missing cookie", s.public.String(), "", token, reasonCSRFCookieMissing, false},
		{"missing token", s.public.String(), token, "", reasonCSRFTokenMissing, false},
		{"wrong token", s.public.String(), token, "different", reasonCSRFTokenMismatch, false},
		{"valid", s.public.String(), token, token, "", true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			got, ok := s.checkCSRF(request(tc.origin, tc.cookie, tc.form), "csrf")
			if got != tc.want || ok != tc.wantOK {
				t.Fatalf("checkCSRF = (%q, %t), want (%q, %t)", got, ok, tc.want, tc.wantOK)
			}
		})
	}
}
func TestCookieWebSocketRequiresExactOrigin(t *testing.T) {
	s := testService(t)
	raw, _, _ := s.createSession("subject", "Name", "local")
	h := s.Enforce(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) { w.WriteHeader(204) }), Listener{})
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
	raw, _, _ := s.createSession("subject", "Name", "local")
	h := s.Enforce(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) { w.WriteHeader(204) }), Listener{})
	r := secureRequest("GET", "/download", nil)
	r.Host = "meter.example:7443"
	r.AddCookie(&http.Cookie{Name: sessionCookie, Value: raw})
	r.Header.Set("Origin", s.public.String())
	r.Header.Set("Sec-Fetch-Site", "same-site")
	rr := httptest.NewRecorder()
	h.ServeHTTP(rr, r)
	if rr.Code != 204 {
		t.Fatalf("code=%d, want 204", rr.Code)
	}
}
func TestCookieMeasurementRejectsSiblingSiteWithoutExactOrigin(t *testing.T) {
	s := testService(t)
	raw, _, _ := s.createSession("subject", "Name", "local")
	h := s.Enforce(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) { w.WriteHeader(204) }), Listener{})
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
			t.Fatalf("origin=%q code=%d, want 403", origin, rr.Code)
		}
	}
}

func TestCookieMeasurementRequiresPositiveSameOriginEvidence(t *testing.T) {
	s := testService(t)
	raw, _, _ := s.createSession("subject", "Name", "local")
	h := s.Enforce(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) { w.WriteHeader(http.StatusNoContent) }), Listener{})
	for _, site := range []string{"", "none", "cross-site"} {
		r := secureRequest(http.MethodGet, "/probe", nil)
		r.AddCookie(&http.Cookie{Name: sessionCookie, Value: raw})
		if site != "" {
			r.Header.Set("Sec-Fetch-Site", site)
		}
		rr := httptest.NewRecorder()
		h.ServeHTTP(rr, r)
		if rr.Code != http.StatusForbidden {
			t.Fatalf("site=%q code=%d, want 403", site, rr.Code)
		}
	}
	r := secureRequest(http.MethodGet, "/probe", nil)
	r.AddCookie(&http.Cookie{Name: sessionCookie, Value: raw})
	r.Header.Set("Sec-Fetch-Site", "same-origin")
	rr := httptest.NewRecorder()
	h.ServeHTTP(rr, r)
	if rr.Code != http.StatusNoContent {
		t.Fatalf("same-origin code=%d, want 204", rr.Code)
	}
}

func TestSuccessfulAuthenticatedResponseHasTransportAndFrameHeaders(t *testing.T) {
	s := testService(t)
	raw, _, _ := s.createSession("subject", "Name", "local")
	h := s.Enforce(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) { w.WriteHeader(http.StatusOK) }), Listener{UI: true})
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
	raw, _, _ := s.createSession("subject", "Name", "local")
	h := s.Enforce(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) { w.WriteHeader(204) }), Listener{UI: true})
	r := secureRequest("GET", "/auth/session", nil)
	r.Host = "meter.example:7443"
	r.AddCookie(&http.Cookie{Name: sessionCookie, Value: raw})
	rr := httptest.NewRecorder()
	h.ServeHTTP(rr, r)
	if rr.Code != http.StatusForbidden {
		t.Fatalf("code=%d, want 403", rr.Code)
	}
}
func TestBearerCannotAccessBrowserRoutes(t *testing.T) {
	s := testService(t)
	_, sess, _ := s.createSession("subject", "Name", "local")
	grant := randomToken(32)
	grantHash := sha256.Sum256([]byte(grant))
	sess.grants[grantHash] = struct{}{}
	s.grants[grantHash] = sess
	h := s.Enforce(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) { w.WriteHeader(204) }), Listener{UI: true})
	r := secureRequest("GET", "/auth/session", nil)
	r.Header.Set("Authorization", "Bearer "+grant)
	rr := httptest.NewRecorder()
	h.ServeHTTP(rr, r)
	if rr.Code != 403 {
		t.Fatalf("code=%d, want 403", rr.Code)
	}
}
func TestAuthRequiredExposesHeadersCrossOrigin(t *testing.T) {
	s := testService(t)
	h := s.Enforce(http.HandlerFunc(func(http.ResponseWriter, *http.Request) { t.Fatal("called") }), Listener{})
	r := secureRequest("GET", "/download", nil)
	r.Header.Set("Origin", s.public.String())
	rr := httptest.NewRecorder()
	h.ServeHTTP(rr, r)
	if rr.Header().Get("Access-Control-Allow-Origin") != s.public.String() || !strings.Contains(rr.Header().Get("Access-Control-Expose-Headers"), "Graphite-Meter-Auth") {
		t.Fatalf("headers=%v", rr.Header())
	}
}
func TestOffModeReservesAuthRoutes(t *testing.T) {
	s, _ := New(t.Context(), config.AuthConfig{Mode: "off"}, nil, false)
	mux := http.NewServeMux()
	s.Mount(mux)
	mux.HandleFunc("/", func(w http.ResponseWriter, _ *http.Request) { w.WriteHeader(200) })
	for _, path := range []string{"/login", "/auth/session"} {
		rr := httptest.NewRecorder()
		mux.ServeHTTP(rr, httptest.NewRequest("GET", path, nil))
		if rr.Code != 404 {
			t.Errorf("%s code=%d, want 404", path, rr.Code)
		}
	}
}
func TestOIDCTransactionCookieAllowsTopLevelCallback(t *testing.T) {
	rr := httptest.NewRecorder()
	setTransactionCookie(rr, "value", time.Now().Add(time.Minute))
	cookies := rr.Result().Cookies()
	if len(cookies) != 1 || cookies[0].SameSite != http.SameSiteLaxMode {
		t.Fatalf("cookies=%+v, want exactly one with SameSite=Lax", cookies)
	}
}
func TestAttemptLimiterStaysBounded(t *testing.T) {
	s := testService(t)
	now := time.Now()
	for i := range 2048 {
		s.attempts[fmt.Sprintf("2001:db8::%x", i)] = loginAttempt{times: []time.Time{now}}
	}
	r := secureRequest("POST", "/auth/password", nil)
	r.RemoteAddr = "192.0.2.1:1234"
	if s.allowAttempt(r) {
		t.Fatal("new entry admitted at capacity")
	}
	if len(s.attempts) != 2048 {
		t.Fatalf("attempts=%d, want 2048", len(s.attempts))
	}
}
func TestPerAddressRejectionsDoNotConsumeGlobalPasswordLimit(t *testing.T) {
	s := testService(t)
	for i := range 61 {
		r := secureRequest(http.MethodPost, "/auth/password", nil)
		r.RemoteAddr = "192.0.2.1:1234"
		want := i < 5
		if got := s.allowAttempt(r); got != want {
			t.Fatalf("attempt %d allowed=%v want=%v", i+1, got, want)
		}
	}
	if len(s.globalAttempts) != 5 {
		t.Fatalf("global attempts=%d, want 5", len(s.globalAttempts))
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
	for i := range 5 {
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
	for i := range 60 {
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
				t.Fatalf("authClientAddress = (%v, %v), want (%q, %v)", addr, ok, tc.want, tc.ok)
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
				s.oidc = newOIDCState(s.cfg, "secret", false)
				if tc.ready {
					s.oidc.provider = &oidc.Provider{}
				}
			}
			rr := httptest.NewRecorder()
			s.loginPage(rr, secureRequest(http.MethodGet, "/login", nil))
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
	s, err := New(t.Context(), config.AuthConfig{Mode: "password", PublicURL: "https://meter.example", PasswordHash: hash, OIDCProviderName: "Authelia"}, nil, false)
	if err != nil {
		t.Fatal(err)
	}
	mux := http.NewServeMux()
	s.Mount(mux)
	handler := s.Enforce(mux, Listener{UI: true})

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
	for i := range maxSubjectSessions + 1 {
		_, sess, err := s.createSession("same", "Name", "local")
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
		t.Fatalf("sessions=%d, want %d", len(s.sessions), maxSubjectSessions)
	}
}
func TestUnknownCLIChallengeAllocatesNothing(t *testing.T) {
	s := testService(t)
	verifier := "not-known"
	body := `{"verifier":"` + verifier + `"}`
	rr := httptest.NewRecorder()
	r := secureRequest("POST", "/auth/cli/token", nil)
	r.Body = io.NopCloser(strings.NewReader(body))
	s.cliToken(rr, r)
	if rr.Code != http.StatusAccepted || len(s.approvals) != 0 {
		t.Fatalf("code=%d approvals=%d, want %d and 0", rr.Code, len(s.approvals), http.StatusAccepted)
	}
	sum := sha256.Sum256([]byte(verifier))
	if _, ok := s.approvals[base64.RawURLEncoding.EncodeToString(sum[:])]; ok {
		t.Fatal("unknown verifier allocated state")
	}
}

func TestCLITokenRejectsDuplicateJSONNames(t *testing.T) {
	s := testService(t)
	verifier := "strict-json-verifier"
	_, sess, err := s.createSession("subject", "Name", "local")
	if err != nil {
		t.Fatal(err)
	}
	challenge := challengeFor(verifier)
	s.approvals[challenge] = &cliApproval{session: sess, expires: sess.expires, approved: true}
	r := secureRequest("POST", "/auth/cli/token", &countingReader{r: bytes.NewReader([]byte(`{"verifier":"unknown","verifier":"` + verifier + `"}`))})
	r.Header.Set("Content-Type", "application/json")
	rr := httptest.NewRecorder()
	s.cliToken(rr, r)
	if rr.Code != http.StatusAccepted {
		t.Fatalf("duplicate-name token request code=%d, want 202", rr.Code)
	}
	if len(sess.grants) != 0 {
		t.Fatal("duplicate-name request issued a grant")
	}
}
func TestVerificationCodeIsAlwaysEightCharacters(t *testing.T) {
	for range 100 {
		challenge := randomToken(32)
		if code := verificationCode(challenge); len(code) != 8 {
			t.Fatalf("code=%q length=%d", code, len(code))
		}
	}
}

// The login page carries a CLI challenge into its forms only when it is
// well-formed.
func TestLoginPageOnlyCarriesValidChallenge(t *testing.T) {
	s := testService(t)
	sum := sha256.Sum256([]byte("terminal-verifier"))
	challenge := base64.RawURLEncoding.EncodeToString(sum[:])
	rr := httptest.NewRecorder()
	s.loginPage(rr, secureRequest(http.MethodGet, "/login?challenge="+challenge, nil))
	if !strings.Contains(rr.Body.String(), challenge) {
		t.Fatalf("valid challenge was dropped: %s", rr.Body.String())
	}
	rr = httptest.NewRecorder()
	s.loginPage(rr, secureRequest(http.MethodGet, "/login?challenge=bogus-challenge", nil))
	if strings.Contains(rr.Body.String(), "bogus-challenge") {
		t.Fatal("invalid challenge was reflected")
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
	s.loginRejected(rr, r, reasonPasswordMismatch)
	location, err := url.Parse(rr.Header().Get("Location"))
	if err != nil || location.Query().Get("challenge") != challenge || location.Query().Get("error") != string(noticePassword) {
		t.Fatalf("location=%q err=%v", rr.Header().Get("Location"), err)
	}
	r.Form.Set("challenge", "invalid")
	rr = httptest.NewRecorder()
	s.loginRejected(rr, r, reasonPasswordMismatch)
	location, _ = url.Parse(rr.Header().Get("Location"))
	if location.Query().Has("challenge") {
		t.Fatal("invalid challenge was reflected")
	}
}
func TestCLIApprovalExchangeIsSingleUseAndRevokedWithSession(t *testing.T) {
	s := testService(t)
	_, sess, _ := s.createSession("subject", "Name", "local")
	verifier := "terminal-verifier"
	sum := sha256.Sum256([]byte(verifier))
	challenge := base64.RawURLEncoding.EncodeToString(sum[:])
	s.approvals[challenge] = &cliApproval{session: sess, expires: time.Now().Add(time.Minute), approved: true}
	exchange := func() *httptest.ResponseRecorder {
		rr := httptest.NewRecorder()
		r := secureRequest("POST", "/auth/cli/token", nil)
		r.Body = io.NopCloser(strings.NewReader(`{"verifier":"` + verifier + `"}`))
		s.cliToken(rr, r)
		return rr
	}
	first := exchange()
	if first.Code != 200 {
		t.Fatalf("first code=%d, want 200; body=%s", first.Code, first.Body.String())
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
		t.Fatalf("replay code=%d, want 202", second.Code)
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
	_, sess, _ := s.createSession("subject", "Name", "local")
	for i := range 20 {
		verifier := fmt.Sprintf("verifier-%d", i)
		sum := sha256.Sum256([]byte(verifier))
		challenge := base64.RawURLEncoding.EncodeToString(sum[:])
		s.approvals[challenge] = &cliApproval{session: sess, expires: time.Now().Add(time.Minute), approved: true}
		rr := httptest.NewRecorder()
		r := secureRequest("POST", "/auth/cli/token", nil)
		r.Body = io.NopCloser(strings.NewReader(`{"verifier":"` + verifier + `"}`))
		s.cliToken(rr, r)
		if rr.Code != 200 {
			t.Fatalf("exchange %d code=%d, want 200", i, rr.Code)
		}
	}
	if len(sess.grants) > 8 {
		t.Fatalf("grants=%d, want at most 8", len(sess.grants))
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
	// auth.css states the light palette twice, per data-theme and as the OS
	// fallback, so consecutive repeats collapse. Drift between them survives.
	collapse := func(in []string) []string {
		out := in[:0:0]
		for _, value := range in {
			if len(out) == 0 || out[len(out)-1] != value {
				out = append(out, value)
			}
		}
		return out
	}
	for _, name := range []string{"canvas", "surface-1", "surface-inset", "border", "text", "text-muted", "text-inverse", "brand", "brand-strong", "signal", "signal-soft", "err", "err-soft", "focus-ring", "edge-highlight"} {
		if authValues, appValues := collapse(values(authCSS, name)), values(string(css), name); !reflect.DeepEqual(authValues, appValues) {
			t.Errorf("token %s values %v do not match application values %v", name, authValues, appValues)
		}
	}
}

func TestGeneratedAuthAssetsAreCurrent(t *testing.T) {
	for path, generated := range map[string]string{
		"../../../client/src/auth/auth.css":      authCSS,
		"../../../client/src/auth/theme.js":      authThemeJS,
		"../../../client/src/auth/pending.js":    authPendingJS,
		"../../../client/src/auth/login.tmpl":    loginHTML,
		"../../../client/src/auth/cli.tmpl":      cliHTML,
		"../../../client/src/auth/cli-done.tmpl": cliDoneHTML,
		"../../../client/src/auth/continue.tmpl": continueHTML,
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
