package auth

import (
	"context"
	"crypto/rand"
	"crypto/rsa"
	"crypto/sha256"
	"encoding/base64"
	"encoding/json"
	"io"
	"net/http"
	"net/http/httptest"
	"net/url"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/go-jose/go-jose/v4"
	"github.com/go-jose/go-jose/v4/jwt"
	"github.com/zR-JB/graphite-meter/go/internal/config"
)

type fakeOIDC struct {
	server         *httptest.Server
	key            *rsa.PrivateKey
	mu             sync.Mutex
	nonce          string
	challenge      string
	audience       string
	subject        string
	userinfoSub    string
	groups         []string
	expires        time.Time
	accessToken    string
	badAccessHash  bool
	badSignature   bool
	tokenStatus    int
	jwksStatus     int
	userinfoStatus int
	discoveries    int
	mistypedMeta   bool
}

func newFakeOIDC(t *testing.T) *fakeOIDC {
	t.Helper()
	key, err := rsa.GenerateKey(rand.Reader, 2048)
	if err != nil {
		t.Fatal(err)
	}
	f := &fakeOIDC{key: key, audience: "client", subject: "subject", userinfoSub: "subject", groups: []string{"allowed"}, expires: time.Now().Add(time.Hour), accessToken: "access-token"}
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
			"issuer": f.server.URL, "authorization_endpoint": f.server.URL + "/authorize", "token_endpoint": f.server.URL + "/token",
			"jwks_uri": f.server.URL + "/jwks", "userinfo_endpoint": f.server.URL + "/userinfo", "authorization_response_iss_parameter_supported": responseIssuer,
		})
	case "/jwks":
		f.mu.Lock()
		status := f.jwksStatus
		f.mu.Unlock()
		if status != 0 {
			http.Error(w, "temporarily unavailable", status)
			return
		}
		writeJSON(w, jose.JSONWebKeySet{Keys: []jose.JSONWebKey{{Key: &f.key.PublicKey, KeyID: "test", Algorithm: string(jose.RS256), Use: "sig"}}})
	case "/token":
		f.mu.Lock()
		status := f.tokenStatus
		f.mu.Unlock()
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
		nonce, challenge, audience, subject, expires, accessToken, badHash, badSignature := f.nonce, f.challenge, f.audience, f.subject, f.expires, f.accessToken, f.badAccessHash, f.badSignature
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
			signingKey, _ = rsa.GenerateKey(rand.Reader, 2048)
		}
		signer, _ := jose.NewSigner(jose.SigningKey{Algorithm: jose.RS256, Key: signingKey}, (&jose.SignerOptions{}).WithType("JWT").WithHeader("kid", "test"))
		raw, _ := jwt.Signed(signer).Claims(map[string]any{"iss": f.server.URL, "aud": audience, "sub": subject, "iat": time.Now().Unix(), "exp": expires.Unix(), "nonce": nonce, "at_hash": atHash}).Serialize()
		writeJSON(w, map[string]any{"access_token": accessToken, "token_type": "Bearer", "expires_in": 3600, "id_token": raw})
	case "/userinfo":
		f.mu.Lock()
		subject, groups, status := f.userinfoSub, append([]string(nil), f.groups...), f.userinfoStatus
		f.mu.Unlock()
		if status != 0 {
			http.Error(w, "temporarily unavailable", status)
			return
		}
		writeJSON(w, map[string]any{"sub": subject, "name": "Example User", "groups": groups})
	default:
		http.NotFound(w, r)
	}
}

func writeJSON(w http.ResponseWriter, value any) {
	w.Header().Set("Content-Type", "application/json")
	_ = json.NewEncoder(w).Encode(value)
}

func (f *fakeOIDC) service(t *testing.T) *Service {
	t.Helper()
	previous := http.DefaultTransport
	http.DefaultTransport = f.server.Client().Transport
	t.Cleanup(func() { http.DefaultTransport = previous })
	ctx, cancel := context.WithCancel(context.Background())
	t.Cleanup(cancel)
	s, err := New(ctx, config.AuthConfig{Mode: "oidc", PublicURL: "https://meter.example", OIDCIssuer: f.server.URL, OIDCClientID: "client", OIDCClientSecret: "secret", OIDCAllowedGroups: []string{"allowed"}, OIDCProviderName: "Provider"}, nil, false)
	if err != nil {
		t.Fatal(err)
	}
	return s
}

func startOIDC(t *testing.T, s *Service, f *fakeOIDC) (state string, cookie *http.Cookie) {
	t.Helper()
	csrf := "abcdefghijklmnopqrstuvwxyz0123456789"
	body := url.Values{"csrf": {csrf}}.Encode()
	r := secureRequest(http.MethodPost, "/auth/oidc/start", nil)
	r.Body = io.NopCloser(strings.NewReader(body))
	r.Header.Set("Content-Type", "application/x-www-form-urlencoded")
	r.Header.Set("Origin", s.public.String())
	r.AddCookie(&http.Cookie{Name: loginCookie, Value: csrf})
	rr := httptest.NewRecorder()
	s.oidcStart(rr, r)
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

func finishOIDC(s *Service, state string, cookie *http.Cookie, query string) *httptest.ResponseRecorder {
	r := secureRequest(http.MethodGet, "/auth/oidc/callback?state="+url.QueryEscape(state)+"&code=valid-code&iss="+url.QueryEscape(s.cfg.OIDCIssuer)+query, nil)
	r.AddCookie(cookie)
	rr := httptest.NewRecorder()
	s.oidcCallback(rr, r)
	return rr
}

func TestOIDCLoginSecurityChecks(t *testing.T) {
	tests := []struct {
		name   string
		mutate func(*fakeOIDC)
		want   int
	}{
		{"valid", func(*fakeOIDC) {}, http.StatusOK},
		{"wrong audience", func(f *fakeOIDC) { f.audience = "other" }, http.StatusSeeOther},
		{"expired", func(f *fakeOIDC) { f.expires = time.Now().Add(-time.Minute) }, http.StatusSeeOther},
		{"userinfo subject mismatch", func(f *fakeOIDC) { f.userinfoSub = "other" }, http.StatusSeeOther},
		{"group case mismatch", func(f *fakeOIDC) { f.groups = []string{"Allowed"} }, http.StatusSeeOther},
		{"bad access hash", func(f *fakeOIDC) { f.badAccessHash = true }, http.StatusSeeOther},
		{"bad signature", func(f *fakeOIDC) { f.badSignature = true }, http.StatusSeeOther},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			f := newFakeOIDC(t)
			test.mutate(f)
			s := f.service(t)
			state, cookie := startOIDC(t, s, f)
			rr := finishOIDC(s, state, cookie, "")
			if rr.Code != test.want {
				t.Fatalf("status=%d, want %d", rr.Code, test.want)
			}
			loggedIn := false
			for _, c := range rr.Result().Cookies() {
				if c.Name == sessionCookie && c.Value != "" {
					loggedIn = true
				}
			}
			if want := test.name == "valid"; loggedIn != want {
				t.Fatalf("loggedIn=%v, want %v", loggedIn, want)
			}
		})
	}
}

func TestOIDCCallbackRejectsWrongNonceAndMissingIssuer(t *testing.T) {
	f := newFakeOIDC(t)
	s := f.service(t)
	state, cookie := startOIDC(t, s, f)
	f.mu.Lock()
	f.nonce = "wrong"
	f.mu.Unlock()
	if rr := finishOIDC(s, state, cookie, ""); !strings.Contains(rr.Header().Get("Location"), "error="+string(noticeGeneric)) {
		t.Fatal("wrong nonce accepted")
	}

	state, cookie = startOIDC(t, s, f)
	r := secureRequest(http.MethodGet, "/auth/oidc/callback?state="+url.QueryEscape(state)+"&code=valid-code", nil)
	r.AddCookie(cookie)
	rr := httptest.NewRecorder()
	s.oidcCallback(rr, r)
	if !strings.Contains(rr.Header().Get("Location"), "error="+string(noticeGeneric)) {
		t.Fatal("missing response issuer accepted")
	}
}

func TestOIDCCallbackRejectsReplayAndDuplicateParameters(t *testing.T) {
	f := newFakeOIDC(t)
	s := f.service(t)
	state, cookie := startOIDC(t, s, f)
	if rr := finishOIDC(s, state, cookie, ""); rr.Code != http.StatusOK {
		t.Fatalf("first status=%d, want 200", rr.Code)
	}
	if rr := finishOIDC(s, state, cookie, ""); !strings.Contains(rr.Header().Get("Location"), "error="+string(noticeGeneric)) {
		t.Fatal("transaction replay accepted")
	}

	state, cookie = startOIDC(t, s, f)
	rr := finishOIDC(s, state, cookie, "&state=duplicate")
	if !strings.Contains(rr.Header().Get("Location"), "error="+string(noticeGeneric)) {
		t.Fatal("duplicate state accepted")
	}
}

func TestOIDCCallbackFailureDoesNotDisableProvider(t *testing.T) {
	for _, endpoint := range []string{"token", "jwks", "userinfo"} {
		t.Run(endpoint, func(t *testing.T) {
			f := newFakeOIDC(t)
			s := f.service(t)
			state, cookie := startOIDC(t, s, f)
			f.mu.Lock()
			switch endpoint {
			case "token":
				f.tokenStatus = http.StatusServiceUnavailable
			case "jwks":
				f.jwksStatus = http.StatusServiceUnavailable
			case "userinfo":
				f.userinfoStatus = http.StatusServiceUnavailable
			}
			f.mu.Unlock()
			_ = finishOIDC(s, state, cookie, "")
			if !s.oidc.ready() {
				t.Fatal("one failed callback disabled OIDC globally")
			}
			f.mu.Lock()
			discoveries := f.discoveries
			f.mu.Unlock()
			if discoveries != 1 {
				t.Fatalf("one failed callback triggered %d discoveries", discoveries)
			}
		})
	}
}

func TestOIDCStartupDoesNotProbeProtectedProviderEndpoints(t *testing.T) {
	f := newFakeOIDC(t)
	f.mu.Lock()
	f.tokenStatus = http.StatusServiceUnavailable
	f.jwksStatus = http.StatusServiceUnavailable
	f.userinfoStatus = http.StatusServiceUnavailable
	f.mu.Unlock()
	s := f.service(t)
	if !s.oidc.ready() {
		t.Fatal("successful discovery did not make OIDC available")
	}
}

// A provider whose discovery document mistypes an optional field still yields
// valid endpoints, so discovery must complete rather than fail permanently.
func TestOIDCDiscoveryToleratesMistypedOptionalMetadata(t *testing.T) {
	f := newFakeOIDC(t)
	f.mu.Lock()
	f.mistypedMeta = true
	f.mu.Unlock()
	s := f.service(t)
	if !s.oidc.ready() {
		t.Fatal("a mistyped optional metadata field disabled OIDC")
	}
	if s.oidc.responseIssuer {
		t.Fatal("responseIssuer decoded from a mistyped field")
	}
}

// The callback is the tail of a navigation the identity provider started. A
// redirect there would be followed without the SameSite=Strict session cookie
// the callback just set, bouncing a successful first login back to /login, so
// the callback must answer with a same-site hop instead.
func TestOIDCCallbackCompletesWithSameSiteHopNotRedirect(t *testing.T) {
	f := newFakeOIDC(t)
	s := f.service(t)
	state, cookie := startOIDC(t, s, f)
	rr := finishOIDC(s, state, cookie, "")
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
	var session *http.Cookie
	for _, c := range rr.Result().Cookies() {
		if c.Name == sessionCookie {
			session = c
		}
	}
	if session == nil || session.SameSite != http.SameSiteStrictMode {
		t.Fatalf("session cookie = %+v, want SameSite=Strict", session)
	}
	if ct := rr.Header().Get("Content-Type"); !strings.HasPrefix(ct, "text/html") {
		t.Fatalf("content-type=%q, want a text/html interstitial", ct)
	}
}

// A CLI challenge carried through the OIDC transaction must reach the approval
// page through the same same-site hop, and only when it is well-formed.
func TestOIDCInterstitialCarriesOnlyValidCLIChallenge(t *testing.T) {
	s := testService(t)
	sum := sha256.Sum256([]byte("terminal-verifier"))
	challenge := base64.RawURLEncoding.EncodeToString(sum[:])

	rr := httptest.NewRecorder()
	s.writeSignedInInterstitial(rr, challenge)
	if !strings.Contains(rr.Body.String(), "/auth/cli?challenge="+challenge) {
		t.Fatalf("valid challenge was dropped: %s", rr.Body.String())
	}

	rr = httptest.NewRecorder()
	s.writeSignedInInterstitial(rr, "not-a-challenge")
	if body := rr.Body.String(); strings.Contains(body, "not-a-challenge") || !strings.Contains(body, `url=/"`) {
		t.Fatalf("invalid challenge was reflected: %s", body)
	}
}
