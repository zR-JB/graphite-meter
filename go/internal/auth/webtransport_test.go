package auth

import (
	"context"
	"crypto/sha256"
	"encoding/base64"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
	"time"
)

// mintForSession runs the mint through the request path: a principal-bearing
// request against MintWebTransportSessionToken.
func mintForSession(t *testing.T, s *Service, sess *session) string {
	t.Helper()
	r := secureRequest(http.MethodPost, "/wt/session", nil)
	p := Principal{Subject: sess.subject, session: sess}
	r = r.WithContext(context.WithValue(r.Context(), principalKey{}, p))
	token, expires, mint := s.MintWebTransportSessionToken(r)
	if mint != WTMintOK || token == "" || !expires.After(s.now()) {
		t.Fatalf("mint = (%q, %v, %d), want a live token", token, expires, mint)
	}
	return token
}

// wtConnect drives the Enforce boundary with an extended CONNECT carrying the
// given URL query, and reports whether the request reached the handler.
func wtConnect(t *testing.T, s *Service, path string) (reached bool, status int) {
	t.Helper()
	reachedHandler := false
	next := http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		reachedHandler = true
		if _, ok := PrincipalFromContext(r.Context()); !ok {
			t.Error("CONNECT reached the handler without a principal")
		}
	})
	// httptest parses a CONNECT target in authority form; the HTTP/3 server
	// delivers an extended CONNECT with a normal :path, which this mirrors.
	r := secureRequest(http.MethodGet, path, nil)
	r.Method = http.MethodConnect
	w := httptest.NewRecorder()
	s.Enforce(next, Listener{WebTransport: true}).ServeHTTP(w, r)
	return reachedHandler, w.Code
}

// A listener that does not mount the session routes must not run the CONNECT
// branch: the request would 404 anyway, and the token is single-use.
func TestWebTransportConnectLeavesTheTokenOnANonSessionListener(t *testing.T) {
	s := testService(t)
	_, sess, err := s.createSession("subject", "Name", "local", time.Time{})
	if err != nil {
		t.Fatalf("create session: %v", err)
	}
	token := mintForSession(t, s, sess)

	r := secureRequest(http.MethodGet, "/wt/download", nil)
	r.Method = http.MethodConnect
	r.URL.RawQuery = "token=" + token
	w := httptest.NewRecorder()
	s.Enforce(http.HandlerFunc(func(http.ResponseWriter, *http.Request) {}), Listener{}).ServeHTTP(w, r)

	// Unspent: the same token still authenticates on the listener that serves it.
	if _, ok := s.consumeWebTransportToken(token); !ok {
		t.Fatal("a CONNECT to a listener without the session routes spent the token")
	}
}

func TestWebTransportConnectAcceptsAMintedTokenOnce(t *testing.T) {
	s := testService(t)
	_, sess, err := s.createSession("subject", "Name", "local", time.Time{})
	if err != nil {
		t.Fatal(err)
	}
	token := mintForSession(t, s, sess)
	if !strings.HasPrefix(token, wtTokenPrefix) {
		t.Fatalf("token %q lacks the %q prefix", token, wtTokenPrefix)
	}

	if reached, status := wtConnect(t, s, "/wt/ping?token="+token); !reached {
		t.Fatalf("minted token was refused: HTTP %d", status)
	}
	// Single use: a replayed CONNECT URL is worthless.
	if reached, status := wtConnect(t, s, "/wt/ping?token="+token); reached || status != http.StatusForbidden {
		t.Fatalf("replayed token: reached=%t status=%d, want a 403 refusal", reached, status)
	}
}

func TestWebTransportConnectRefusesWithoutACredential(t *testing.T) {
	s := testService(t)
	for _, path := range []string{"/wt/ping", "/wt/download?bytes=0", "/wt/upload?token=gmw_bogus"} {
		reached, status := wtConnect(t, s, path)
		if reached || status != http.StatusForbidden {
			t.Fatalf("CONNECT %s: reached=%t status=%d, want a 403 refusal before upgrade", path, reached, status)
		}
	}
}

// The CONNECT path applies none of the origin, Sec-Fetch-Site or double-submit
// rules that make an ambient credential safe, so it must take none. A live
// session cookie is refused there even though the same cookie authenticates
// every other route.
func TestWebTransportConnectRefusesASessionCookie(t *testing.T) {
	s := testService(t)
	raw, sess, err := s.createSession("subject", "Name", "local", time.Time{})
	if err != nil {
		t.Fatalf("create session: %v", err)
	}
	// The cookie is live: it authenticates a request-shaped measurement route.
	if _, ok := s.authenticate(withSessionCookie(secureRequest(http.MethodPost, "/upload", nil), raw)); !ok {
		t.Fatal("the test cookie does not authenticate at all")
	}

	reached := false
	next := http.HandlerFunc(func(http.ResponseWriter, *http.Request) { reached = true })
	r := withSessionCookie(secureRequest(http.MethodGet, "/wt/ping", nil), raw)
	r.Method = http.MethodConnect
	w := httptest.NewRecorder()
	s.Enforce(next, Listener{WebTransport: true}).ServeHTTP(w, r)
	if reached || w.Code != http.StatusForbidden {
		t.Fatalf("cookie-only CONNECT: reached=%t status=%d, want a 403 refusal", reached, w.Code)
	}
	// The refusal is the cookie being ignored, not the session being unusable:
	// a token minted from it still gets in.
	if reached, status := wtConnect(t, s, "/wt/ping?token="+mintForSession(t, s, sess)); !reached {
		t.Fatalf("minted token refused after the cookie was: HTTP %d", status)
	}
}

func TestWebTransportTokensDieWithTheirSession(t *testing.T) {
	s := testService(t)
	_, sess, err := s.createSession("subject", "Name", "local", time.Time{})
	if err != nil {
		t.Fatal(err)
	}
	token := mintForSession(t, s, sess)
	s.mu.Lock()
	s.deleteSessionLocked(sess)
	s.mu.Unlock()
	if _, ok := s.consumeWebTransportToken(token); ok {
		t.Fatal("token outlived its revoked session")
	}
}

func TestWebTransportTokensExpireAndCapPerSession(t *testing.T) {
	s := testService(t)
	_, sess, err := s.createSession("subject", "Name", "local", time.Time{})
	if err != nil {
		t.Fatal(err)
	}
	stale := mintForSession(t, s, sess)
	base := time.Now()
	offset := wtTokenLifetime + time.Second
	s.now = func() time.Time { return base.Add(offset) }
	if _, ok := s.consumeWebTransportToken(stale); ok {
		t.Fatal("expired token accepted")
	}

	// At the cap the mint refuses. Evicting instead would spend a token another
	// tab is about to present, turning one dial's retry into another's failure.
	tokens := make([]string, 0, maxSessionWTTokens)
	for range maxSessionWTTokens {
		offset += time.Second
		tokens = append(tokens, mintForSession(t, s, sess))
	}
	if len(sess.wtTokens) != maxSessionWTTokens {
		t.Fatalf("session holds %d tokens, want the %d cap", len(sess.wtTokens), maxSessionWTTokens)
	}
	r := secureRequest(http.MethodPost, "/wt/session", nil)
	r = r.WithContext(context.WithValue(r.Context(), principalKey{}, Principal{Subject: sess.subject, session: sess}))
	if _, _, mint := s.MintWebTransportSessionToken(r); mint == WTMintOK {
		t.Fatal("mint past the cap succeeded")
	}
	// Every token the cap protected is still spendable.
	for i, token := range tokens {
		if _, ok := s.consumeWebTransportToken(token); !ok {
			t.Fatalf("token %d refused after a mint hit the cap", i)
		}
	}
	// Consuming them frees the cap, so a client that finishes its dials can mint.
	offset += time.Second
	mintForSession(t, s, sess)
}

// grantFor issues a native-client grant against sess, as cliToken does: the
// grant and the browser login that approved it are one session.
func grantFor(t *testing.T, s *Service, sess *session) string {
	t.Helper()
	grant := randomToken(32)
	h := sha256.Sum256([]byte(grant))
	s.mu.Lock()
	defer s.mu.Unlock()
	sess.grants[h] = struct{}{}
	s.grants[h] = sess
	return grant
}

// mintWithGrant drives the mint through the boundary the way anything holding a
// grant would reach it, and reports whether a token came back.
func mintWithGrant(t *testing.T, s *Service, grant string) bool {
	t.Helper()
	minted := false
	next := http.HandlerFunc(func(_ http.ResponseWriter, r *http.Request) {
		_, _, mint := s.MintWebTransportSessionToken(r)
		minted = mint == WTMintOK
	})
	r := secureRequest(http.MethodPost, "/wt/session", nil)
	r.Header.Set("Authorization", "Bearer "+grant)
	w := httptest.NewRecorder()
	s.Enforce(next, Listener{}).ServeHTTP(w, r)
	if w.Code == http.StatusForbidden {
		t.Fatalf("the grant did not reach the mint at all: HTTP %d", w.Code)
	}
	return minted
}

// A grant carries the session its browser approval created, so a token minted
// from one occupies that login's cap. Nothing needs it to: a native CONNECT
// presents its Authorization header directly. Minting from a grant would let
// anything holding one exhaust the eight slots and deny the browser it came
// from, for as long as it kept re-minting.
func TestWebTransportMintRefusesABearerGrant(t *testing.T) {
	s := testService(t)
	_, sess, err := s.createSession("subject", "Name", "local", time.Time{})
	if err != nil {
		t.Fatalf("create session: %v", err)
	}
	grant := grantFor(t, s, sess)

	for i := range maxSessionWTTokens + 1 {
		if mintWithGrant(t, s, grant) {
			t.Fatalf("mint %d from a bearer grant returned a token", i+1)
		}
	}
	if len(sess.wtTokens) != 0 {
		t.Fatalf("a grant parked %d tokens on the login's session", len(sess.wtTokens))
	}
	// The starvation the refusal prevents: the browser's own mint still lands.
	if reached, status := wtConnect(t, s, "/wt/ping?token="+mintForSession(t, s, sess)); !reached {
		t.Fatalf("the login could not dial after its own grant minted: HTTP %d", status)
	}
}

// A refusal at the cap is capacity, not an authentication failure: the session
// is intact and a slot frees within wtTokenLifetime. The two refusals must be
// distinguishable, or the endpoint answers both with the same status and a
// signed-in caller is told to retry what will never work, or to log in again.
func TestWebTransportMintSeparatesCapacityFromNoSession(t *testing.T) {
	s := testService(t)
	_, sess, err := s.createSession("subject", "Name", "local", time.Time{})
	if err != nil {
		t.Fatalf("create session: %v", err)
	}
	for range maxSessionWTTokens {
		mintForSession(t, s, sess)
	}
	r := secureRequest(http.MethodPost, "/wt/session", nil)
	r = r.WithContext(context.WithValue(r.Context(), principalKey{}, Principal{Subject: sess.subject, session: sess}))
	if _, _, mint := s.MintWebTransportSessionToken(r); mint != WTMintAtCapacity {
		t.Fatalf("mint at the cap = %d, want WTMintAtCapacity (%d)", mint, WTMintAtCapacity)
	}
	if _, _, mint := s.MintWebTransportSessionToken(secureRequest(http.MethodPost, "/wt/session", nil)); mint != WTMintNoSession {
		t.Fatalf("mint without a principal = %d, want WTMintNoSession (%d)", mint, WTMintNoSession)
	}
}

// expireLocked runs on a wtTokenLifetime ticker, so a session that reaches its
// deadline keeps its entry, and its tokens theirs, until the next sweep. The
// deadline is what must stop them authenticating, not the sweep.
func TestWebTransportTokensDieWithAnExpiredSession(t *testing.T) {
	s := testService(t)
	_, sess, err := s.createSession("subject", "Name", "local", time.Now().Add(50*time.Millisecond))
	if err != nil {
		t.Fatalf("create session: %v", err)
	}
	token := mintForSession(t, s, sess)
	select {
	case <-sess.ctx.Done():
	case <-time.After(5 * time.Second):
		t.Fatal("session never reached its deadline")
	}
	h := sha256.Sum256([]byte(token))
	s.mu.Lock()
	_, listed := s.wtTokens[h]
	s.mu.Unlock()
	if !listed {
		t.Fatal("the token was already swept, so this no longer covers the deadline")
	}
	if _, ok := s.consumeWebTransportToken(token); ok {
		t.Fatal("a token minted before the deadline authenticated after it")
	}
}

// Revocation removes the token, rather than leaving consumeWebTransportToken's
// session check to refuse it: an unremoved token holds a slot in the service
// map for its full lifetime after the session that owned it is gone.
func TestRevokingASessionRemovesItsTokensFromTheService(t *testing.T) {
	s := testService(t)
	_, sess, err := s.createSession("subject", "Name", "local", time.Time{})
	if err != nil {
		t.Fatalf("create session: %v", err)
	}
	h := sha256.Sum256([]byte(mintForSession(t, s, sess)))
	s.mu.Lock()
	s.deleteSessionLocked(sess)
	_, listed := s.wtTokens[h]
	s.mu.Unlock()
	if listed {
		t.Fatal("a revoked session's token stayed in the service map")
	}
}

// The CONNECT branch runs ahead of the boundary's own TLS demand, so it makes
// its own: the token is a credential in a URL and must not cross a cleartext
// hop. secureRequest sets r.TLS, so nothing else in this package drives one.
func TestWebTransportConnectRefusesCleartext(t *testing.T) {
	s := testService(t)
	_, sess, err := s.createSession("subject", "Name", "local", time.Time{})
	if err != nil {
		t.Fatalf("create session: %v", err)
	}
	token := mintForSession(t, s, sess)

	reached := false
	next := http.HandlerFunc(func(http.ResponseWriter, *http.Request) { reached = true })
	r := secureRequest(http.MethodGet, "/wt/ping?token="+token, nil)
	r.Method = http.MethodConnect
	r.TLS = nil
	w := httptest.NewRecorder()
	s.Enforce(next, Listener{WebTransport: true}).ServeHTTP(w, r)
	if reached || w.Code != http.StatusForbidden {
		t.Fatalf("cleartext CONNECT: reached=%t status=%d, want a 403 refusal", reached, w.Code)
	}
	// Refused before the credential was read, so it is still spendable.
	if _, ok := s.consumeWebTransportToken(token); !ok {
		t.Fatal("a cleartext CONNECT spent the token it was refused for")
	}
}

// The token is the whole credential for a session upgrade and it travels in a
// URL, so its length is a security property in its own right.
func TestWebTransportTokenCarries256Bits(t *testing.T) {
	s := testService(t)
	_, sess, err := s.createSession("subject", "Name", "local", time.Time{})
	if err != nil {
		t.Fatalf("create session: %v", err)
	}
	raw, ok := strings.CutPrefix(mintForSession(t, s, sess), wtTokenPrefix)
	if !ok {
		t.Fatalf("minted token lacks the %q prefix", wtTokenPrefix)
	}
	decoded, err := base64.RawURLEncoding.DecodeString(raw)
	if err != nil {
		t.Fatalf("minted token is not base64url: %v", err)
	}
	if len(decoded) != 32 {
		t.Fatalf("token carries %d bytes, want the 32 every other credential here carries", len(decoded))
	}
}

// The session rows of allowedCORSMethod are the one part of that table no
// browser reaches: CONNECT is a forbidden fetch method, so it never appears in
// a preflight, and corsPreflight's own Access-Control-Allow-Methods omits it.
// They exist because routes_test.go requires every pinned measurement path to
// allow some method, which is loose enough that MethodPost satisfies it just as
// well. This says which method, so the rows describe the routes they name.
func TestWebTransportSessionRoutesPreflightForCONNECTAlone(t *testing.T) {
	for _, path := range []string{"/wt/download", "/wt/upload", "/wt/ping"} {
		for _, method := range []string{http.MethodGet, http.MethodHead, http.MethodPost, http.MethodPut, http.MethodDelete, http.MethodOptions} {
			if allowedCORSMethod(path, method) {
				t.Errorf("allowedCORSMethod(%q, %q) = true; a session route is reached by extended CONNECT and by nothing else", path, method)
			}
		}
		if !allowedCORSMethod(path, http.MethodConnect) {
			t.Errorf("allowedCORSMethod(%q, CONNECT) = false", path)
		}
	}
	// The mint beside them is the ordinary POST it looks like.
	if !allowedCORSMethod("/wt/session", http.MethodPost) || allowedCORSMethod("/wt/session", http.MethodConnect) {
		t.Error("/wt/session is the plain POST mint, not a session upgrade")
	}
}

// Stated, not derived: the expiry tests offset from this constant, so a change
// to it would validate itself.
func TestWebTransportTokenLifetimeIsThirtySeconds(t *testing.T) {
	if wtTokenLifetime != 30*time.Second {
		t.Fatalf("wtTokenLifetime = %v, want 30s: a token outliving the dial it was minted for holds its session's cap for nothing", wtTokenLifetime)
	}
}
