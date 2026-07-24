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
	s.Enforce(next, Listener{}).ServeHTTP(w, r)
	return reachedHandler, w.Code
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

	// Distinct expiries make the eviction order deterministic: oldest first.
	tokens := make([]string, 0, maxSessionWTTokens+1)
	for range maxSessionWTTokens + 1 {
		offset += time.Second
		tokens = append(tokens, mintForSession(t, s, sess))
	}
	if len(sess.wtTokens) != maxSessionWTTokens {
		t.Fatalf("session holds %d tokens, want the %d cap", len(sess.wtTokens), maxSessionWTTokens)
	}
	if _, ok := s.consumeWebTransportToken(tokens[0]); ok {
		t.Fatal("evicted token accepted")
	}
	if _, ok := s.consumeWebTransportToken(tokens[len(tokens)-1]); !ok {
		t.Fatal("newest token refused")
	}
}
