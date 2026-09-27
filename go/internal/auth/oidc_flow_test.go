package auth

import (
	"context"
	"crypto/rand"
	"crypto/rsa"
	"crypto/sha256"
	"encoding/base64"
	"fmt"
	"net/http"
	"net/http/httptest"
	"net/url"
	"slices"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/coreos/go-oidc/v3/oidc"
	"github.com/go-jose/go-jose/v4"
	"github.com/go-jose/go-jose/v4/jwt"
	"github.com/zR-JB/graphite-meter/go/internal/config"
	"github.com/zR-JB/graphite-meter/go/internal/testkit"
)

type fakeOIDC struct {
	server        *httptest.Server
	key           *rsa.PrivateKey
	mu            sync.Mutex
	nonce         string
	challenge     string
	audience      string
	subject       string
	userinfoSub   string
	groups        []string
	expires       time.Time
	accessToken   string
	badAccessHash bool
	badSignature  bool
	tokenStatus   int
	tokenRedirect bool
	hugeUserinfo  bool
	discoveries   int
	mistypedMeta  bool
}

// Signing keys are generated once per package; each takes a noticeable fraction of a second.
var providerKey, strangerKey = sync.OnceValue(newRSAKey), sync.OnceValue(newRSAKey)

func newRSAKey() *rsa.PrivateKey {
	key, err := rsa.GenerateKey(rand.Reader, 2048)
	if err != nil {
		panic(err)
	}
	return key
}

func newFakeOIDC(t *testing.T) *fakeOIDC {
	t.Helper()
	f := &fakeOIDC{key: providerKey(), audience: "client", subject: "subject", userinfoSub: "subject",
		groups: []string{"allowed"}, expires: time.Now().Add(time.Hour), accessToken: "access-token"}
	f.server = httptest.NewTLSServer(http.HandlerFunc(f.serveHTTP))
	t.Cleanup(f.server.Close)
	return f
}

func (f *fakeOIDC) serveHTTP(w http.ResponseWriter, r *http.Request) {
	switch r.URL.Path {
	case "/.well-known/openid-configuration":
		f.mu.Lock()
		f.discoveries++
		mistyped := f.mistypedMeta
		f.mu.Unlock()
		var responseIssuer any = true
		if mistyped {
			responseIssuer = "yes"
		}
		writeJSON(w, map[string]any{
			"issuer":                 f.server.URL,
			"authorization_endpoint": f.server.URL + "/authorize",
			"token_endpoint":         f.server.URL + "/token",
			"jwks_uri":               f.server.URL + "/jwks",
			"userinfo_endpoint":      f.server.URL + "/userinfo",
			"authorization_response_iss_parameter_supported": responseIssuer,
		})
	case "/jwks":
		key := jose.JSONWebKey{Key: &f.key.PublicKey, KeyID: "test", Algorithm: string(jose.RS256), Use: "sig"}
		writeJSON(w, jose.JSONWebKeySet{Keys: []jose.JSONWebKey{key}})
	case "/token":
		f.mu.Lock()
		status, redirect := f.tokenStatus, f.tokenRedirect
		f.mu.Unlock()
		if redirect && r.URL.Query().Get("hop") == "" {
			http.Redirect(w, r, "/token?hop=1", http.StatusTemporaryRedirect)
			return
		}
		if status != 0 {
			http.Error(w, "temporarily unavailable", status)
			return
		}
		if user, secret, ok := r.BasicAuth(); !ok || user != "client" || secret != "secret" {
			http.Error(w, "invalid client", http.StatusUnauthorized)
			return
		}
		if err := r.ParseForm(); err != nil || r.Form.Get("code") != "valid-code" || r.Form.Get("code_verifier") == "" {
			http.Error(w, "invalid request", http.StatusBadRequest)
			return
		}
		f.mu.Lock()
		nonce, challenge, audience, subject, expires, accessToken, badHash, badSignature := f.nonce, f.challenge,
			f.audience, f.subject, f.expires, f.accessToken, f.badAccessHash, f.badSignature
		f.mu.Unlock()
		verifierHash := sha256.Sum256([]byte(r.Form.Get("code_verifier")))
		if base64.RawURLEncoding.EncodeToString(verifierHash[:]) != challenge {
			http.Error(w, "invalid verifier", http.StatusBadRequest)
			return
		}
		hash := sha256.Sum256([]byte(accessToken))
		atHash := base64.RawURLEncoding.EncodeToString(hash[:len(hash)/2])
		if badHash {
			atHash = "invalid"
		}
		signingKey := f.key
		if badSignature {
			signingKey = strangerKey()
		}
		signer, _ := jose.NewSigner(jose.SigningKey{Algorithm: jose.RS256, Key: signingKey},
			(&jose.SignerOptions{}).WithType("JWT").WithHeader("kid", "test"))
		raw, _ := jwt.Signed(signer).Claims(map[string]any{"iss": f.server.URL, "aud": audience, "sub": subject,
			"iat": time.Now().Unix(), "exp": expires.Unix(), "nonce": nonce, "at_hash": atHash}).Serialize()
		writeJSON(w, map[string]any{
			"access_token": accessToken, "token_type": "Bearer", "expires_in": 3600, "id_token": raw,
		})
	case "/userinfo":
		f.mu.Lock()
		subject, groups, huge := f.userinfoSub, slices.Clone(f.groups), f.hugeUserinfo
		f.mu.Unlock()
		padding := ""
		if huge {
			padding = strings.Repeat("x", 1<<20)
		}
		writeJSON(w, map[string]any{"sub": subject, "name": "Example User", "groups": groups, "padding": padding})
	default:
		http.NotFound(w, r)
	}
}

func (f *fakeOIDC) service(t *testing.T) *Service {
	t.Helper()
	ctx, cancel := context.WithCancel(oidc.ClientContext(t.Context(), f.server.Client()))
	t.Cleanup(cancel)
	s, err := New(ctx, config.AuthConfig{
		Mode: "oidc", PublicURL: "https://meter.example", OIDCIssuer: f.server.URL, OIDCClientID: "client",
		OIDCClientSecret: "secret", OIDCAllowedGroups: []string{"allowed"}, OIDCProviderName: "Provider",
	}, nil, false)
	if err != nil {
		t.Fatal(err)
	}
	return s
}

func oidcStartRequest(s *Service, challenge string) *http.Request {
	const csrf = "abcdefghijklmnopqrstuvwxyz0123456789"
	form := url.Values{"csrf": {csrf}}
	if challenge != "" {
		form.Set("challenge", challenge)
	}
	r := secureRequest(http.MethodPost, "/auth/oidc/start", strings.NewReader(form.Encode()))
	r.Header.Set("Content-Type", "application/x-www-form-urlencoded")
	r.Header.Set("Origin", s.origin)
	r.AddCookie(&http.Cookie{Name: loginCookie, Value: csrf})
	return r
}

func startOIDC(t *testing.T, s *Service, f *fakeOIDC, r *http.Request) (string, *http.Cookie) {
	t.Helper()
	rr := testkit.Record(s.oidcStart, r)
	if rr.Code != http.StatusSeeOther {
		t.Fatalf("start status=%d, want 303", rr.Code)
	}
	location, err := url.Parse(rr.Header().Get("Location"))
	if err != nil {
		t.Fatal(err)
	}
	f.mu.Lock()
	f.nonce = location.Query().Get("nonce")
	f.challenge = location.Query().Get("code_challenge")
	f.mu.Unlock()
	for _, c := range rr.Result().Cookies() {
		if c.Name == transactionCookie {
			return location.Query().Get("state"), c
		}
	}
	t.Fatal("transaction cookie missing")
	return "", nil
}

// finishOIDC returns from the provider with a valid code and issuer, as edited by edit when it is not nil.
func finishOIDC(s *Service, state string, cookie *http.Cookie, edit func(url.Values)) *httptest.ResponseRecorder {
	query := url.Values{"state": {state}, "code": {"valid-code"}, "iss": {s.cfg.OIDCIssuer}}
	if edit != nil {
		edit(query)
	}
	r := secureRequest(http.MethodGet, "/auth/oidc/callback?"+query.Encode(), nil)
	r.AddCookie(cookie)
	rr := testkit.Record(s.oidcCallback, r)
	return rr
}

func sessionCookieIn(rr *httptest.ResponseRecorder) *http.Cookie {
	for _, c := range rr.Result().Cookies() {
		if c.Name == sessionCookie && c.Value != "" {
			return c
		}
	}
	return nil
}

// One client holds at most its share of pending sign-ins, so it cannot exhaust the transaction table.
func TestOIDCTransactionsAreBoundedPerClient(t *testing.T) {
	s := newFakeOIDC(t).service(t)
	start := func(remote string) string {
		r := oidcStartRequest(s, "")
		r.RemoteAddr = remote
		rr := testkit.Record(s.oidcStart, r)
		return rr.Header().Get("Location")
	}
	for i := range maxClientOIDCTransactions {
		if location := start(fmt.Sprintf("[2001:db8:1:2::%x]:40000", i)); strings.HasPrefix(location, "/login") {
			t.Fatalf("sign-in %d refused: %s", i, location)
		}
	}
	if location := start("[2001:db8:1:2::ff]:40000"); location != "/login?error=busy" {
		t.Fatalf("sign-in over the client's share = %q, want a busy refusal", location)
	}
	if location := start("[2001:db8:1:3::1]:40000"); strings.HasPrefix(location, "/login") {
		t.Fatalf("another client was refused: %s", location)
	}
	s.oidc.mu.Lock()
	for i := len(s.oidc.tx); i < maxOIDCTransactions; i++ {
		s.oidc.tx[[32]byte{byte(i), byte(i >> 8)}] = oidcTransaction{expires: time.Now().Add(time.Hour)}
	}
	s.oidc.mu.Unlock()
	if location := start("[2001:db8:1:4::1]:40000"); location != "/login?error=busy" {
		t.Fatalf("sign-in past the global table = %q, want a busy refusal", location)
	}
}

// Every refused callback shows only the generic notice and leaves the provider usable without rediscovery.
func TestOIDCLoginSecurityChecks(t *testing.T) {
	repeat := func(key string) func(url.Values) { return func(q url.Values) { q.Add(key, q.Get(key)) } }
	tests := []struct {
		name   string
		mutate func(*fakeOIDC)
		edit   func(url.Values)
		replay bool
		expire bool
	}{
		{"valid", nil, nil, false, false},
		{"wrong audience", func(f *fakeOIDC) { f.audience = "other" }, nil, false, false},
		{"expired", func(f *fakeOIDC) { f.expires = time.Now().Add(-time.Minute) }, nil, false, false},
		{"userinfo subject mismatch", func(f *fakeOIDC) { f.userinfoSub = "other" }, nil, false, false},
		{"group case mismatch", func(f *fakeOIDC) { f.groups = []string{"Allowed"} }, nil, false, false},
		{"bad access hash", func(f *fakeOIDC) { f.badAccessHash = true }, nil, false, false},
		{"bad signature", func(f *fakeOIDC) { f.badSignature = true }, nil, false, false},
		{"wrong nonce", func(f *fakeOIDC) { f.nonce = "wrong" }, nil, false, false},
		{"token endpoint unavailable", func(f *fakeOIDC) { f.tokenStatus = http.StatusServiceUnavailable }, nil,
			false, false},
		{"token endpoint redirects", func(f *fakeOIDC) { f.tokenRedirect = true }, nil, false, false},
		{"userinfo past 1 MiB", func(f *fakeOIDC) { f.hugeUserinfo = true }, nil, false, false},
		{"missing issuer", nil, func(q url.Values) { q.Del("iss") }, false, false},
		{"duplicate state", nil, repeat("state"), false, false},
		{"duplicate code", nil, repeat("code"), false, false},
		{"replay", nil, nil, true, false},
		{"expired transaction", nil, nil, false, true},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			f := newFakeOIDC(t)
			s := f.service(t)
			state, cookie := startOIDC(t, s, f, oidcStartRequest(s, ""))
			if test.mutate != nil {
				f.mu.Lock()
				test.mutate(f)
				f.mu.Unlock()
			}
			if test.expire {
				for k, tx := range s.oidc.tx {
					tx.expires = time.Now().Add(-time.Second)
					s.oidc.tx[k] = tx
				}
			}
			if test.replay {
				if rr := finishOIDC(s, state, cookie, nil); rr.Code != http.StatusOK {
					t.Fatalf("first callback status=%d, want 200", rr.Code)
				}
			}
			rr := finishOIDC(s, state, cookie, test.edit)
			loggedIn := sessionCookieIn(rr) != nil
			if test.name == "valid" {
				if rr.Code != http.StatusOK || !loggedIn {
					t.Fatalf("status=%d loggedIn=%t, want 200 with a session", rr.Code, loggedIn)
				}
			} else if rr.Code != http.StatusSeeOther || loggedIn ||
				!strings.Contains(rr.Header().Get("Location"), "error="+string(noticeGeneric)) {
				t.Fatalf("status=%d location=%q loggedIn=%t, want the generic refusal", rr.Code,
					rr.Header().Get("Location"), loggedIn)
			}
			f.mu.Lock()
			discoveries := f.discoveries
			f.mu.Unlock()
			if !s.oidc.ready() || discoveries != 1 {
				t.Fatalf("provider ready=%t after %d discoveries, want ready after one", s.oidc.ready(), discoveries)
			}
		})
	}
}

// Signing in again replaces the browser's previous login and ends the grants it delegated.
func TestOIDCLoginRevokesTheSessionItReplaces(t *testing.T) {
	f := newFakeOIDC(t)
	s := f.service(t)
	prior, sess, err := s.createSession("local-operator", "Local operator", "local")
	if err != nil {
		t.Fatal(err)
	}
	grant := grantFor(t, s, sess)
	state, cookie := startOIDC(t, s, f, withSessionCookie(oidcStartRequest(s, ""), prior))
	if rr := finishOIDC(s, state, cookie, nil); rr.Code != http.StatusOK {
		t.Fatalf("callback status=%d, want 200", rr.Code)
	}
	if _, ok := s.authenticateGrant(grant); ok || s.sessions[sess.hash] != nil || len(s.sessions) != 1 {
		t.Fatal("the new login kept the session it replaced or that session's grant")
	}
}

func TestOIDCDiscoveryToleratesMistypedOptionalMetadata(t *testing.T) {
	f := newFakeOIDC(t)
	f.mu.Lock()
	f.mistypedMeta = true
	f.mu.Unlock()
	s := f.service(t)
	if !s.oidc.ready() {
		t.Fatal("a mistyped optional metadata field disabled OIDC")
	}
	if s.oidc.discovered.Load().responseIssuer {
		t.Fatal("responseIssuer decoded from a mistyped field")
	}
}

func TestOIDCCallbackCompletesWithSameSiteHopNotRedirect(t *testing.T) {
	f := newFakeOIDC(t)
	s := f.service(t)
	state, cookie := startOIDC(t, s, f, oidcStartRequest(s, ""))
	rr := finishOIDC(s, state, cookie, nil)
	if rr.Code != http.StatusOK {
		t.Fatalf("status=%d, want 200", rr.Code)
	}
	if location := rr.Header().Get("Location"); location != "" {
		t.Fatalf("callback redirected to %q; a cross-site hop drops the Strict session cookie", location)
	}
	body := rr.Body.String()
	if !strings.Contains(body, `http-equiv="refresh"`) || !strings.Contains(body, `url=/"`) {
		t.Fatalf("interstitial does not navigate to the application root: %s", body)
	}
	if session := sessionCookieIn(rr); session == nil || session.SameSite != http.SameSiteStrictMode {
		t.Fatalf("session cookie = %+v, want SameSite=Strict", session)
	}
}

// The callback continues to a browser approval only for the valid challenge its sign-in carried.
func TestOIDCLoginReturnsToTheBrowserApproval(t *testing.T) {
	f := newFakeOIDC(t)
	s := f.service(t)
	challenge := challengeFor(randomToken(32))
	path := "/auth/browser?" + url.Values{"challenge": {challenge}, "client_origin": {requestingUI}}.Encode()
	if w := serveMounted(s, secureRequest(http.MethodGet, path, nil)); w.Code != http.StatusSeeOther {
		t.Fatalf("browser approval did not request login: %d", w.Code)
	}
	state, transaction := startOIDC(t, s, f, oidcStartRequest(s, challenge))
	loggedIn := finishOIDC(s, state, transaction, nil)
	login := sessionCookieIn(loggedIn)
	if login == nil || !strings.Contains(loggedIn.Body.String(), "/auth/cli?challenge="+challenge) {
		t.Fatalf("OIDC lost the approval continuation: %d", loggedIn.Code)
	}
	cli := secureRequest(http.MethodGet, "/auth/cli?challenge="+challenge, nil)
	cli.AddCookie(login)
	if w := serveMounted(s, cli); w.Code != http.StatusSeeOther || w.Header().Get("Location") != path {
		t.Fatalf("wrong browser approval destination: %d %s", w.Code, w.Header().Get("Location"))
	}
	page := secureRequest(http.MethodGet, path, nil)
	page.AddCookie(login)
	if w := serveMounted(s, page); w.Code != http.StatusOK || !strings.Contains(w.Body.String(), requestingUI) ||
		!strings.Contains(w.Body.String(), "/auth/browser/approve") || s.approvals[challenge].approved {
		t.Fatal("OIDC continuation did not require explicit browser-origin approval")
	}
	state, transaction = startOIDC(t, s, f, oidcStartRequest(s, "not-a-challenge"))
	if body := finishOIDC(s, state, transaction, nil).Body.String(); strings.Contains(body, "challenge=") {
		t.Fatalf("an invalid challenge survived the sign-in: %s", body)
	}
}
