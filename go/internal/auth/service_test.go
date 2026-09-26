package auth

import (
	"bytes"
	"context"
	"crypto/sha256"
	"crypto/tls"
	"encoding/base64"
	"html/template"
	"io"
	"net/http"
	"net/http/httptest"
	"os"
	"reflect"
	"regexp"
	"slices"
	"strings"
	"testing"
	"testing/synctest"
	"time"

	"github.com/zR-JB/graphite-meter/go/internal/config"
	"golang.org/x/oauth2"
)

func testService(t *testing.T) *Service {
	t.Helper()
	return serviceWithin(t, t.Context())
}

// quietService never sweeps, so each request's own expiry check is the only one that can refuse.
func quietService(t *testing.T) *Service {
	t.Helper()
	ctx, stop := context.WithCancel(t.Context())
	s := serviceWithin(t, ctx)
	stop()
	return s
}

func serviceWithin(t *testing.T, ctx context.Context) *Service {
	t.Helper()
	h, err := HashPassword("secret")
	if err != nil {
		t.Fatal(err)
	}
	s, err := New(ctx, config.AuthConfig{
		Mode: "password", PublicURL: "https://meter.example", PasswordHash: h, OIDCProviderName: "Authelia",
	}, nil, false)
	if err != nil {
		t.Fatal(err)
	}
	return s
}

func secureRequest(method, path string, body io.Reader) *http.Request {
	r := httptest.NewRequest(method, "https://meter.example"+path, body)
	r.Host = "meter.example"
	r.TLS = &tls.ConnectionState{}
	return r
}

func withSessionCookie(r *http.Request, raw string) *http.Request {
	r.AddCookie(&http.Cookie{Name: sessionCookie, Value: raw})
	return r
}

func statusHandler(code int) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) { w.WriteHeader(code) })
}

type countingReader struct {
	r io.Reader
	n int
}

func (r *countingReader) Read(p []byte) (int, error) {
	n, err := r.r.Read(p)
	r.n += n
	return n, err
}

func TestOffModeIsTransparentAndReservesAuthRoutes(t *testing.T) {
	s, err := New(t.Context(), config.AuthConfig{Mode: "off"}, nil, false)
	if err != nil {
		t.Fatal(err)
	}
	rr := httptest.NewRecorder()
	s.Enforce(statusHandler(299), Listener{}).ServeHTTP(rr, httptest.NewRequest("GET", "http://example/anything", nil))
	if rr.Code != 299 {
		t.Fatalf("code=%d, want the wrapped handler's 299", rr.Code)
	}
	mux := http.NewServeMux()
	s.Mount(mux)
	mux.Handle("/", statusHandler(200))
	for _, path := range []string{"/login", "/auth/session"} {
		rr := httptest.NewRecorder()
		mux.ServeHTTP(rr, httptest.NewRequest("GET", path, nil))
		if rr.Code != 404 {
			t.Errorf("%s code=%d, want 404", path, rr.Code)
		}
	}
}

func TestUnauthenticatedRequestsAreRefusedBeforeTheirBody(t *testing.T) {
	s := testService(t)
	for _, tc := range []struct {
		name, method, path string
		listener           Listener
		want               int
	}{
		{"UI root", http.MethodGet, "/", Listener{UI: true}, http.StatusTemporaryRedirect},
		{"measurement root", http.MethodGet, "/", Listener{}, http.StatusForbidden},
		{"UI API", http.MethodGet, "/preflight", Listener{UI: true}, http.StatusForbidden},
		{"upload", http.MethodPost, "/upload", Listener{}, http.StatusForbidden},
	} {
		t.Run(tc.name, func(t *testing.T) {
			body := &countingReader{r: bytes.NewReader(make([]byte, 1024))}
			h := s.Enforce(http.HandlerFunc(func(http.ResponseWriter, *http.Request) { t.Fatal("called") }),
				tc.listener)
			rr := httptest.NewRecorder()
			h.ServeHTTP(rr, secureRequest(tc.method, tc.path, body))
			if rr.Code != tc.want || body.n != 0 || rr.Header().Get("Connection") != "close" {
				t.Fatalf("code=%d bytes=%d connection=%q, want %d before reading and a closed H1 connection",
					rr.Code, body.n, rr.Header().Get("Connection"), tc.want)
			}
			if want := s.origin + "/login"; tc.want == http.StatusTemporaryRedirect &&
				rr.Header().Get("Location") != want {
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
	ended := make(chan bool, 1)
	h := s.Enforce(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		close(entered)
		<-r.Context().Done()
		ended <- SessionEnded(r.Context())
	}), Listener{})
	r := withSessionCookie(secureRequest("GET", "/download", nil), raw)
	r.Header.Set("Sec-Fetch-Site", "same-origin")
	go h.ServeHTTP(httptest.NewRecorder(), r)
	<-entered
	s.mu.Lock()
	s.deleteSessionLocked(sess)
	s.mu.Unlock()
	select {
	case sessionEnded := <-ended:
		if !sessionEnded {
			t.Fatal("session cancellation cause was not preserved")
		}
	case <-time.After(time.Second):
		t.Fatal("request context was not cancelled")
	}
	ctx, cancel := context.WithTimeout(t.Context(), 0)
	defer cancel()
	if SessionEnded(ctx) {
		t.Fatal("ordinary operation deadline classified as session expiry")
	}
}

func TestRequestEvidencePolicy(t *testing.T) {
	s := testService(t)
	raw, sess, err := s.createSession("subject", "Name", "local")
	if err != nil {
		t.Fatal(err)
	}
	grant := grantFor(t, s, sess)
	for _, tc := range []struct {
		name, method, path, host, origin, site, csrf string
		bearer                                       bool
		want                                         int
	}{
		{"mutation without origin", "POST", "/upload", "", "", "", sess.csrf, false, 403},
		{"mutation without CSRF", "POST", "/upload", "", s.origin, "", "", false, 403},
		{"mutation with the session token as CSRF", "POST", "/upload", "", s.origin, "", raw, false, 403},
		{"mutation with origin and CSRF", "POST", "/upload", "", s.origin, "", sess.csrf, false, 204},
		{"WebSocket without origin", "GET", "/ws/ping", "", "", "", "", false, 403},
		{"WebSocket from another origin", "GET", "/ws/ping", "", "https://wrong.example", "", "", false, 403},
		{"WebSocket from the origin", "GET", "/ws/ping", "", s.origin, "", "", false, 204},
		{"alternate port with exact origin", "GET", "/download", "meter.example:7443", s.origin, "same-site", "", false,
			204},
		{"sibling site without origin", "GET", "/download", "", "", "same-site", "", false, 403},
		{"sibling site with its origin", "GET", "/download", "", "https://evil.example", "same-site", "", false, 403},
		{"no fetch metadata", "GET", "/probe", "", "", "", "", false, 403},
		{"user-initiated", "GET", "/probe", "", "", "none", "", false, 403},
		{"cross-site", "GET", "/probe", "", "", "cross-site", "", false, 403},
		{"same-origin", "GET", "/probe", "", "", "same-origin", "", false, 204},
		{"cross-site page read", "GET", "/", "", "", "cross-site", "", false, 403},
		{"sibling page read without origin", "GET", "/", "", "", "same-site", "", false, 403},
		{"sibling page read with its origin", "GET", "/", "", "https://sibling.meter.example", "same-site", "", false,
			403},
		{"same-origin page read", "GET", "/", "", "", "same-origin", "", false, 204},
		{"page read without fetch metadata", "GET", "/", "", "", "", "", false, 204},
		{"cookie at another hostname", "GET", "/download", "other.example", "", "same-origin", "", false, 403},
		{"grant at the public hostname", "GET", "/download", "", "", "", "", true, 204},
		{"grant at another hostname", "GET", "/download", "other.example", "", "", "", true, 403},
		{"account route on another port", "GET", "/auth/session", "meter.example:7443", "", "", "", false, 403},
		{"account route with a grant", "GET", "/auth/session", "", "", "", "", true, 403},
	} {
		t.Run(tc.name, func(t *testing.T) {
			r := secureRequest(tc.method, tc.path, nil)
			if tc.host != "" {
				r.Host = tc.host
			}
			if tc.bearer {
				r.Header.Set("Authorization", "Bearer "+grant)
			} else {
				withSessionCookie(r, raw)
			}
			for name, value := range map[string]string{"Origin": tc.origin, "Sec-Fetch-Site": tc.site,
				"X-CSRF-Token": tc.csrf} {
				if value != "" {
					r.Header.Set(name, value)
				}
			}
			rr := httptest.NewRecorder()
			s.Enforce(statusHandler(204), Listener{UI: true}).ServeHTTP(rr, r)
			if rr.Code != tc.want {
				t.Fatalf("code=%d, want %d", rr.Code, tc.want)
			}
		})
	}
}

func TestAuthenticatedResponseHeaders(t *testing.T) {
	s := testService(t)
	s.SetConnectOrigins([]string{
		"https://[2001:db8::1]:7248", "wss://[2001:db8::1]:7247", "https://meter.example:*", "wss://meter.example:*",
	})
	if policy := s.PagePolicy(); !strings.HasSuffix(policy,
		"connect-src 'self' https://meter.example:* wss://meter.example:*") {
		t.Fatalf("page policy lost a connect origin or kept an IPv6 literal: %s", policy)
	}
	raw, _, _ := s.createSession("subject", "Name", "local")
	r := withSessionCookie(secureRequest(http.MethodGet, "/download", nil), raw)
	r.Header.Set("Origin", "https://meter.example")
	rr := httptest.NewRecorder()
	s.Enforce(statusHandler(http.StatusOK), Listener{UI: true}).ServeHTTP(rr, r)
	if h := rr.Header(); rr.Code != http.StatusOK || h.Get("Strict-Transport-Security") == "" ||
		h.Get("Referrer-Policy") != "same-origin" || h.Get("Content-Security-Policy") != "" ||
		h.Get("X-Frame-Options") != "" {
		t.Fatalf("headers=%v", rr.Header())
	}
}

func TestLoginCSPAllowsOnlyDiscoveredAuthorizationOrigin(t *testing.T) {
	s := testService(t)
	s.oidc = &oidcState{}
	policy := func(authURL string) string {
		s.oidc.discovered.Store(&oidcDiscovery{oauth: oauth2.Config{Endpoint: oauth2.Endpoint{AuthURL: authURL}}})
		h := http.Header{}
		s.loginSecurityHeaders(h)
		return h.Get("Content-Security-Policy")
	}
	if p := policy("https://login.example:8443/oauth2/authorize"); !strings.Contains(p,
		"form-action 'self' https://login.example:8443;") || strings.Contains(p, "/oauth2/authorize") {
		t.Fatalf("CSP must admit exactly the authorization origin: %q", p)
	}
	if p := policy("http://login.example/authorize"); strings.Contains(p, "login.example") {
		t.Fatalf("clear authorization origin accepted in CSP: %q", p)
	}
}

var inlineScript = regexp.MustCompile(`(?s)<script[^>]*>(.*?)</script>`)

func TestAuthPagesCarryTheScriptPinnedByCSP(t *testing.T) {
	digest := func(asset string) string {
		sum := sha256.Sum256([]byte(asset))
		return "'sha256-" + base64.StdEncoding.EncodeToString(sum[:]) + "'"
	}
	pin := "script-src " + digest(authThemeJS) + " " + digest(authPendingJS)
	if policy := authPageCSP(""); !strings.Contains(policy, pin) || !strings.Contains(policy, "connect-src 'self'") {
		t.Fatalf("CSP %q does not pin both embedded scripts and the same-origin sign-in fetch", policy)
	}
	pages := map[string]struct {
		tmpl *template.Template
		data any
	}{
		"login": {loginTemplate, loginView{Styles: authStyles, Password: true, OIDC: true, Provider: "Provider"}},
		"cli": {cliTemplate,
			map[string]any{"Styles": authStyles, "Code": "ABCD-1234", "Challenge": "c", "CSRF": "t"}},
		"cli-done": {cliDoneTemplate, map[string]any{"Styles": authStyles}},
		"continue": {continueTemplate, map[string]any{"Styles": authStyles, "Challenge": "c"}},
	}
	for name, page := range pages {
		var rendered bytes.Buffer
		if err := page.tmpl.Execute(&rendered, page.data); err != nil {
			t.Fatalf("%s: %v", name, err)
		}
		if (name == "login" || name == "cli") && !strings.Contains(rendered.String(), authPendingJS) {
			t.Errorf("%s submits a form without the pending-state script", name)
		}
		scripts := inlineScript.FindAllStringSubmatch(rendered.String(), -1)
		if len(scripts) == 0 {
			t.Fatalf("%s renders no inline script", name)
		}
		for _, script := range scripts {
			if script[1] != authThemeJS && script[1] != authPendingJS {
				t.Fatalf("%s script does not match a digest pinned in the CSP", name)
			}
		}
	}
}

func TestLoginCSRFFailureReasons(t *testing.T) {
	s := testService(t)
	token := "abcdefghijklmnopqrstuvwxyz0123456789"
	for _, tc := range []struct {
		name, origin, cookie, form string
		want                       reason
	}{
		{"missing origin", "", token, token, reasonCSRFOriginMissing},
		{"null origin", "null", token, token, reasonCSRFOriginMismatch},
		{"wrong origin", "https://wrong.example", token, token, reasonCSRFOriginMismatch},
		{"missing cookie", s.origin, "", token, reasonCSRFCookieMissing},
		{"missing token", s.origin, token, "", reasonCSRFTokenMissing},
		{"wrong token", s.origin, token, "different", reasonCSRFTokenMismatch},
		{"valid", s.origin, token, token, ""},
	} {
		t.Run(tc.name, func(t *testing.T) {
			r := httptest.NewRequest(http.MethodPost, s.origin+"/auth/password", nil)
			r.Form = map[string][]string{"csrf": {tc.form}}
			if tc.origin != "" {
				r.Header.Set("Origin", tc.origin)
			}
			if tc.cookie != "" {
				r.AddCookie(&http.Cookie{Name: loginCookie, Value: tc.cookie})
			}
			got, ok := s.checkCSRF(r, "csrf")
			if got != tc.want || ok != (tc.want == "") {
				t.Fatalf("checkCSRF = (%q, %t), want %q", got, ok, tc.want)
			}
		})
	}
}

// Only an unauthenticated refusal or a browser grant's own origin is exposed to another origin.
func TestOtherOriginsSeeOnlyTheSignInHandshake(t *testing.T) {
	s := testService(t)
	h := s.Enforce(http.HandlerFunc(func(http.ResponseWriter, *http.Request) { t.Fatal("called") }), Listener{})
	for _, origin := range []string{s.origin, "https://other.example", "http://other.example", "null", ""} {
		r := secureRequest("GET", "/download", nil)
		r.Header.Set("Origin", origin)
		rr := httptest.NewRecorder()
		h.ServeHTTP(rr, r)
		if rr.Code != http.StatusForbidden {
			t.Fatalf("origin %q: status %d", origin, rr.Code)
		}
		allowOrigin, credentials := rr.Header().Get("Access-Control-Allow-Origin"),
			rr.Header().Get("Access-Control-Allow-Credentials")
		if !strings.HasPrefix(origin, "https://") {
			if allowOrigin != "" || credentials != "" {
				t.Fatalf("untrusted origin %q exposed by headers=%v", origin, rr.Header())
			}
			continue
		}
		if allowOrigin != origin ||
			!strings.Contains(rr.Header().Get("Access-Control-Expose-Headers"), "Graphite-Meter-Auth") {
			t.Fatalf("headers=%v", rr.Header())
		}
		if origin != s.origin && credentials != "" {
			t.Fatal("cross-origin session cookies were enabled")
		}
	}
	_, sess, _ := s.createSession("subject", "Name", "local")
	for _, bearer := range []bool{false, true} {
		r := secureRequest(http.MethodGet, "/download", nil)
		r.Header.Set("Origin", requestingUI)
		r = r.WithContext(context.WithValue(r.Context(), principalKey{}, sessionPrincipal(sess, "local", bearer)))
		h := http.Header{}
		s.MeasurementCORS(h, r)
		if h.Get("Access-Control-Allow-Origin") != "" {
			t.Errorf("authenticated (bearer=%t) response exposed to %s: %v", bearer, requestingUI, h)
		}
	}
}

func TestLoginOffersOnlyConfiguredMethods(t *testing.T) {
	for _, tc := range []struct {
		mode               string
		password, provider bool
	}{
		{"password", true, false},
		{"oidc", false, true},
		{"hybrid", true, true},
	} {
		t.Run(tc.mode, func(t *testing.T) {
			s := &Service{cfg: config.AuthConfig{Mode: tc.mode, OIDCProviderName: "Provider"}}
			mux := http.NewServeMux()
			s.Mount(mux)
			for _, route := range []struct {
				pattern string
				want    bool
			}{
				{"POST /auth/password", tc.password},
				{"POST /auth/oidc/start", tc.provider},
				{"GET /auth/oidc/callback", tc.provider},
			} {
				method, path, _ := strings.Cut(route.pattern, " ")
				if _, got := mux.Handler(httptest.NewRequest(method, path, nil)); (got == route.pattern) != route.want {
					t.Fatalf("%s mounted as %q", route.pattern, got)
				}
			}
			rr := httptest.NewRecorder()
			s.loginPage(rr, secureRequest(http.MethodGet, "/login", nil))
			body := rr.Body.String()
			if strings.Contains(body, `action="/auth/password"`) != tc.password ||
				strings.Contains(body, `action="/auth/oidc/start"`) != tc.provider {
				t.Fatalf("unexpected methods in %s", body)
			}
		})
	}
}

func TestPerSubjectSessionLimitRevokesOldest(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
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
			time.Sleep(time.Millisecond)
		}
		if oldest.ctx.Err() == nil || len(s.sessions) != maxSubjectSessions {
			t.Fatalf("oldest revoked = %v with %d sessions, want it revoked and %d kept",
				oldest.ctx.Err() != nil, len(s.sessions), maxSubjectSessions)
		}
	})
}

func TestLoginCarriesOnlyAValidChallenge(t *testing.T) {
	s := testService(t)
	challenge := challengeFor("terminal-verifier")
	for _, tc := range []struct {
		challenge string
		carried   bool
	}{{challenge, true}, {"bogus-challenge", false}} {
		rr := httptest.NewRecorder()
		s.loginPage(rr, secureRequest(http.MethodGet, "/login?challenge="+tc.challenge, nil))
		if strings.Contains(rr.Body.String(), tc.challenge) != tc.carried {
			t.Fatalf("login page carried %q = %t", tc.challenge, !tc.carried)
		}
		r := secureRequest(http.MethodPost, "/auth/password", nil)
		r.Form = map[string][]string{"challenge": {tc.challenge}}
		rr = httptest.NewRecorder()
		s.loginRejected(rr, r, reasonPasswordMismatch)
		location := rr.Header().Get("Location")
		if strings.Contains(location, "challenge="+tc.challenge) != tc.carried ||
			!strings.Contains(location, "error=password") {
			t.Fatalf("refusal for %q redirected to %q", tc.challenge, location)
		}
	}
}

func TestLoginPaletteMatchesApplicationTokens(t *testing.T) {
	css, err := os.ReadFile("../../../client/src/app.css")
	if err != nil {
		t.Fatal(err)
	}
	values := func(source, name string) []string {
		re := regexp.MustCompile(`--` + regexp.QuoteMeta(name) + `:\s*([^;]+);`)
		var out []string
		for _, match := range re.FindAllStringSubmatch(source, -1) {
			out = append(out, strings.TrimSpace(match[1]))
		}
		return out
	}
	for _, name := range []string{"canvas", "surface-1", "surface-inset", "border", "text", "text-muted",
		"text-inverse",
		"brand", "brand-strong", "signal", "signal-soft", "err", "err-soft", "focus-ring", "edge-highlight"} {
		authValues := slices.Compact(values(authCSS, name))
		if appValues := values(string(css), name); !reflect.DeepEqual(authValues, appValues) {
			t.Errorf("token %s values %v do not match application values %v", name, authValues, appValues)
		}
	}
}
