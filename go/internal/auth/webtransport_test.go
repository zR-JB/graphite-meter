package auth

import (
	"context"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
	"time"
)

// mintForSession runs the mint through the request path: a principal-bearing
// request against MintWebTransportToken.
func mintForSession(t *testing.T, s *Service, sess *session) string {
	t.Helper()
	r := secureRequest(http.MethodPost, "/wt/session", nil)
	p := Principal{Subject: sess.subject, session: sess}
	r = r.WithContext(context.WithValue(r.Context(), principalKey{}, p))
	token, expires, ok := s.MintWebTransportToken(r)
	if !ok || token == "" || !expires.After(s.now()) {
		t.Fatalf("mint = (%q, %v, %t), want a live token", token, expires, ok)
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
	if _, _, ok := s.MintWebTransportToken(r); ok {
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
