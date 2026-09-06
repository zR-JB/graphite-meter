package auth

import (
	"context"
	"crypto/sha256"
	"encoding/base64"
	"encoding/json/v2"
	"io"
	"net/http"
	"net/http/httptest"
	"net/url"
	"strings"
	"testing"
	"time"
)

const requestingUI = "https://console.example"

func browserExchangeRequest(verifier, origin string) *http.Request {
	r := secureRequest(http.MethodPost, "/auth/browser/token", nil)
	r.Body = io.NopCloser(strings.NewReader(`{"verifier":"` + verifier + `"}`))
	r.Header.Set("Origin", origin)
	r.Header.Set("Content-Type", "application/json")
	return r
}

// Exercise the real approval boundary with a first-party login cookie, then exchange without cookies.
func approveBrowser(t *testing.T, s *Service, cookie string, sess *session) (grant, verifier string) {
	t.Helper()
	mux := http.NewServeMux()
	s.Mount(mux)
	handler := s.Enforce(mux, Listener{UI: true})
	verifier = randomToken(32)
	sum := sha256.Sum256([]byte(verifier))
	challenge := base64.RawURLEncoding.EncodeToString(sum[:])
	path := "/auth/browser?" + url.Values{"challenge": {challenge}, "client_origin": {requestingUI}}.Encode()
	w := httptest.NewRecorder()
	handler.ServeHTTP(w, secureRequest(http.MethodGet, path, nil))
	if w.Code != http.StatusSeeOther || w.Header().Get("Location") != "/login?challenge="+challenge {
		t.Fatalf("login continuation: %d %s", w.Code, w.Header().Get("Location"))
	}
	// Both password and OIDC preserve challenge through the existing /auth/cli continuation.
	w = httptest.NewRecorder()
	handler.ServeHTTP(w, secureRequest(http.MethodGet, "/auth/cli?challenge="+challenge, nil))
	if w.Code != http.StatusSeeOther || !strings.HasPrefix(w.Header().Get("Location"), "/auth/browser?") {
		t.Fatal("login lost the browser approval audience")
	}
	w = httptest.NewRecorder()
	handler.ServeHTTP(w, withSessionCookie(secureRequest(http.MethodGet, path, nil), cookie))
	if w.Code != http.StatusOK || !strings.Contains(w.Body.String(), requestingUI) {
		t.Fatalf("approval did not identify the exact audience: %d %s", w.Code, w.Body.String())
	}
	w = httptest.NewRecorder()
	handler.ServeHTTP(w, browserExchangeRequest(verifier, requestingUI))
	if w.Code != http.StatusAccepted {
		t.Fatalf("unapproved exchange: %d", w.Code)
	}
	r := withSessionCookie(secureRequest(http.MethodPost, "/auth/browser/approve", nil), cookie)
	r.Body = io.NopCloser(strings.NewReader(url.Values{"challenge": {challenge}, "csrf": {sess.csrf}}.Encode()))
	r.Header.Set("Content-Type", "application/x-www-form-urlencoded")
	r.Header.Set("Origin", s.PublicOrigin())
	w = httptest.NewRecorder()
	handler.ServeHTTP(w, r)
	if w.Code != http.StatusOK {
		t.Fatalf("approve: %d", w.Code)
	}
	w = httptest.NewRecorder()
	handler.ServeHTTP(w, browserExchangeRequest(verifier, "https://wrong.example"))
	if w.Code != http.StatusAccepted {
		t.Fatalf("wrong audience exchange: %d", w.Code)
	}
	w = httptest.NewRecorder()
	handler.ServeHTTP(w, browserExchangeRequest(verifier, requestingUI))
	if w.Code != http.StatusOK || w.Header().Get("Access-Control-Allow-Origin") != requestingUI || w.Header().Get("Access-Control-Allow-Credentials") != "" {
		t.Fatalf("cookie-free exchange failed: %d %v", w.Code, w.Header())
	}
	var result struct {
		Token   string `json:"token"`
		Expires int64  `json:"expires"`
	}
	if err := json.Unmarshal(w.Body.Bytes(), &result); err != nil || result.Token == "" || result.Expires != sess.expires.UnixMilli() {
		t.Fatalf("invalid grant: %v %s", err, w.Body.String())
	}
	w = httptest.NewRecorder()
	handler.ServeHTTP(w, browserExchangeRequest(verifier, requestingUI))
	if w.Code != http.StatusAccepted {
		t.Fatalf("approval replay: %d", w.Code)
	}
	return result.Token, verifier
}

func browserBearerRequest(path, grant, origin string) *http.Request {
	r := secureRequest(http.MethodGet, path, nil)
	r.Header.Set("Origin", origin)
	r.Header.Set("Authorization", "Bearer "+grant)
	return r
}

func TestBrowserApprovalKeepsGrantAndCookieScopesSeparate(t *testing.T) {
	s := testService(t)
	raw, sess, err := s.createSession("subject", "Name", "local")
	if err != nil {
		t.Fatal(err)
	}
	grant, _ := approveBrowser(t, s, raw, sess)
	native := grantFor(t, s, sess)
	handler := s.Enforce(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		p, ok := PrincipalFromContext(r.Context())
		if !ok || p.LoginID() != sess.id {
			t.Fatal("grant lost parent admission identity")
		}
		w.WriteHeader(299)
	}), Listener{UI: true})
	for _, tc := range []struct {
		name, path, grant, origin string
		cookie                    bool
		want                      int
	}{
		{"browser discovery", "/preflight", grant, requestingUI, false, 299},
		{"browser upload", "/upload", grant, requestingUI, false, 299},
		{"wrong origin", "/preflight", grant, "https://wrong.example", false, 403},
		{"absent origin", "/preflight", grant, "", false, 403},
		{"catalogue forbidden", "/servers", grant, requestingUI, false, 403},
		{"account forbidden", "/auth/session", grant, requestingUI, false, 403},
		{"cross-site cookies", "/preflight", "", requestingUI, true, 403},
		{"native still rejects remote browser", "/preflight", native, requestingUI, false, 403},
		{"native still works", "/preflight", native, "", false, 299},
	} {
		t.Run(tc.name, func(t *testing.T) {
			r := browserBearerRequest(tc.path, tc.grant, tc.origin)
			if tc.cookie {
				r.Header.Del("Authorization")
				r = withSessionCookie(r, raw)
			}
			w := httptest.NewRecorder()
			handler.ServeHTTP(w, r)
			if w.Code != tc.want {
				t.Fatalf("status=%d, want %d", w.Code, tc.want)
			}
		})
	}
	second, _ := approveBrowser(t, s, raw, sess)
	p, _ := s.authenticateGrant(grant)
	q, _ := s.authenticateGrant(second)
	if p.MeasurementOwner() == q.MeasurementOwner() || p.LoginID() != q.LoginID() {
		t.Fatal("upload access or parent budget is not correctly scoped")
	}
}

func TestOIDCLoginReturnsToTheExactBrowserApproval(t *testing.T) {
	f := newFakeOIDC(t)
	s := f.service(t)
	verifier := randomToken(32)
	sum := sha256.Sum256([]byte(verifier))
	challenge := base64.RawURLEncoding.EncodeToString(sum[:])
	path := "/auth/browser?" + url.Values{"challenge": {challenge}, "client_origin": {requestingUI}}.Encode()
	w := httptest.NewRecorder()
	s.browserPage(w, secureRequest(http.MethodGet, path, nil))
	if w.Code != http.StatusSeeOther {
		t.Fatalf("browser approval did not request login: %d", w.Code)
	}
	state, transaction := startOIDC(t, s, f, challenge)
	loggedIn := finishOIDC(s, state, transaction, "")
	if loggedIn.Code != http.StatusOK || !strings.Contains(loggedIn.Body.String(), "/auth/cli?challenge="+challenge) {
		t.Fatalf("OIDC lost the approval continuation: %d", loggedIn.Code)
	}
	var login *http.Cookie
	for _, cookie := range loggedIn.Result().Cookies() {
		if cookie.Name == sessionCookie {
			login = cookie
		}
	}
	if login == nil {
		t.Fatal("OIDC did not establish a parent login")
	}
	r := secureRequest(http.MethodGet, "/auth/cli?challenge="+challenge, nil)
	r.AddCookie(login)
	w = httptest.NewRecorder()
	s.cliPage(w, r)
	if w.Code != http.StatusSeeOther || w.Header().Get("Location") != path {
		t.Fatalf("wrong browser approval destination: %d %s", w.Code, w.Header().Get("Location"))
	}
	r = secureRequest(http.MethodGet, path, nil)
	r.AddCookie(login)
	w = httptest.NewRecorder()
	s.browserPage(w, r)
	if w.Code != http.StatusOK || !strings.Contains(w.Body.String(), requestingUI) || !strings.Contains(w.Body.String(), "/auth/browser/approve") {
		t.Fatal("OIDC continuation did not require explicit browser-origin approval")
	}
}

func TestBrowserGrantCannotOutliveItsParentLogin(t *testing.T) {
	s := testService(t)
	raw, sess, err := s.createSession("subject", "Name", "local")
	if err != nil {
		t.Fatal(err)
	}
	grant, _ := approveBrowser(t, s, raw, sess)
	p, ok := s.authenticateGrant(grant)
	if !ok {
		t.Fatal("live browser grant was refused")
	}
	s.now = func() time.Time { return sess.expires.Add(time.Second) }
	if _, ok := s.authenticateGrant(grant); ok {
		t.Fatal("expired parent left a browser grant usable")
	}
	// Advancing the test clock does not fire the real context deadline; exercise
	// the same expiry cleanup used by the session sweeper.
	s.mu.Lock()
	s.expireLocked(s.now())
	s.mu.Unlock()
	if p.measurementContext().Err() == nil {
		t.Fatal("expiry did not cancel work admitted by the grant")
	}
}

func TestBrowserSocketTicketsBindAllBoundariesAndRevokeActiveWork(t *testing.T) {
	s := testService(t)
	raw, sess, err := s.createSession("subject", "Name", "local")
	if err != nil {
		t.Fatal(err)
	}
	grant, _ := approveBrowser(t, s, raw, sess)
	mint := func(path string) string {
		t.Helper()
		r := browserBearerRequest("/ws/session?target="+url.QueryEscape("https://meter.example"+path), grant, requestingUI)
		r.Method = http.MethodPost
		var token string
		h := s.Enforce(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
			var status WTMint
			if strings.HasPrefix(path, "/wt/") {
				token, _, status = s.MintWebTransportSessionToken(r)
			} else {
				token, _, status = s.MintWebSocketSessionToken(r)
			}
			if status != WTMintOK {
				t.Fatalf("mint: %d", status)
			}
		}), Listener{})
		w := httptest.NewRecorder()
		h.ServeHTTP(w, r)
		if token == "" {
			t.Fatalf("mint refused: %d", w.Code)
		}
		return token
	}
	for _, tc := range []struct {
		name, path, host, origin string
		want                     bool
	}{
		{"valid", "/ws/ping", "meter.example", requestingUI, true},
		{"route", "/wt/ping", "meter.example", requestingUI, false},
		{"destination", "/ws/ping", "meter.example:8443", requestingUI, false},
		{"origin", "/ws/ping", "meter.example", "https://wrong.example", false},
	} {
		t.Run(tc.name, func(t *testing.T) {
			token := mint("/ws/ping")
			r := secureRequest(http.MethodGet, tc.path, nil)
			r.Host = tc.host
			r.Header.Set("Origin", tc.origin)
			if _, ok := s.consumeWebTransportToken(token, r); ok != tc.want {
				t.Fatalf("ticket accepted=%t", ok)
			}
			if _, ok := s.consumeWebTransportToken(token, r); ok {
				t.Fatal("replayed ticket accepted")
			}
		})
	}
	token := mint("/wt/ping")
	r := secureRequest(http.MethodGet, "/wt/ping?token="+token, nil)
	r.Method = http.MethodConnect
	r.Header.Set("Origin", requestingUI)
	started, ended := make(chan struct{}), make(chan struct{})
	h := s.Enforce(http.HandlerFunc(func(_ http.ResponseWriter, r *http.Request) {
		close(started)
		<-r.Context().Done()
		if !SessionEnded(r.Context()) {
			t.Error("lost revocation cause")
		}
		close(ended)
	}), Listener{WebTransport: true})
	go h.ServeHTTP(httptest.NewRecorder(), r)
	select {
	case <-started:
	case <-time.After(time.Second):
		t.Fatal("ticket did not reach socket")
	}
	s.mu.Lock()
	s.deleteSessionLocked(sess)
	s.mu.Unlock()
	select {
	case <-ended:
	case <-time.After(time.Second):
		t.Fatal("logout left socket running")
	}
	if _, ok := s.authenticateGrant(grant); ok {
		t.Fatal("grant survived logout")
	}
}

func TestBrowserApprovalRejectsInsecureAndNonCanonicalAudiences(t *testing.T) {
	s := testService(t)
	for _, origin := range []string{"http://console.example", "https://console.example/", "https://console.example:443", "null", "https://*.example"} {
		r := secureRequest(http.MethodGet, "/auth/browser?"+url.Values{"client_origin": {origin}, "challenge": {base64.RawURLEncoding.EncodeToString(make([]byte, 32))}}.Encode(), nil)
		w := httptest.NewRecorder()
		s.browserPage(w, r)
		if w.Code != 403 {
			t.Errorf("audience %q status=%d", origin, w.Code)
		}
	}
	// An expired grant context also prevents ticket minting even before periodic cleanup.
	_, sess, _ := s.createSession("subject", "Name", "local")
	ctx, cancel := context.WithCancel(sess.ctx)
	cancel()
	p := sessionPrincipal(sess, "browser", true)
	p.browserGrant = &browserGrant{sess: sess, ctx: ctx}
	r := secureRequest(http.MethodPost, "/wt/session?target=https://meter.example/wt/ping", nil)
	r = r.WithContext(context.WithValue(r.Context(), principalKey{}, p))
	if _, _, status := s.MintWebTransportSessionToken(r); status != WTMintNoSession {
		t.Fatal("revoked grant minted a ticket")
	}
}
