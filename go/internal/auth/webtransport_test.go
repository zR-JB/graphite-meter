package auth

import (
	"context"
	"crypto/sha256"
	"net/http"
	"net/http/httptest"
	"net/url"
	"testing"
	"testing/synctest"
	"time"

	"github.com/zR-JB/graphite-meter/go/internal/route"
)

func mintForSession(t *testing.T, s *Service, sess *session) string {
	t.Helper()
	r := secureRequest(http.MethodPost, "/wt/session?target=https://meter.example/wt/ping", nil)
	p := Principal{Subject: sess.subject, session: sess}
	r = r.WithContext(context.WithValue(r.Context(), principalKey{}, p))
	token, expires, mint := s.MintSocketToken(r, route.WebTransport)
	if mint != SocketMintOK || token == "" || !expires.After(time.Now()) {
		t.Fatalf("mint = (%q, %v, %d), want a live token", token, expires, mint)
	}
	return token
}

func wtConnect(t *testing.T, s *Service, path string) (reached bool, status int) {
	t.Helper()
	reachedHandler := false
	next := http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		reachedHandler = true
		if _, ok := PrincipalFromContext(r.Context()); !ok {
			t.Error("CONNECT reached the handler without a principal")
		}
	})
	r := secureRequest(http.MethodGet, path, nil)
	r.Method = http.MethodConnect
	w := httptest.NewRecorder()
	s.Enforce(next, Listener{WebTransport: true}).ServeHTTP(w, r)
	return reachedHandler, w.Code
}

func TestWebTransportConnectLeavesTheTokenOnANonSessionListener(t *testing.T) {
	s := testService(t)
	_, sess, err := s.createSession("subject", "Name", "local")
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
	if _, ok := s.consumeSocketToken(token, secureRequest(http.MethodGet, "/wt/ping", nil)); !ok {
		t.Fatal("a CONNECT to a listener without the session routes spent the token")
	}
}

func TestWebTransportConnectRefusesASessionCookie(t *testing.T) {
	s := testService(t)
	raw, sess, err := s.createSession("subject", "Name", "local")
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
	// The refusal is the cookie being ignored, not the session being unusable: a token minted from it still gets in.
	if reached, status := wtConnect(t, s, "/wt/ping?token="+mintForSession(t, s, sess)); !reached {
		t.Fatalf("minted token refused after the cookie was: HTTP %d", status)
	}
}

func TestWebTransportTokensDieWithTheirSession(t *testing.T) {
	s := testService(t)
	_, sess, err := s.createSession("subject", "Name", "local")
	if err != nil {
		t.Fatal(err)
	}
	token := mintForSession(t, s, sess)
	s.mu.Lock()
	s.deleteSessionLocked(sess)
	_, listed := s.socketTokens[sha256.Sum256([]byte(token))]
	s.mu.Unlock()
	if listed {
		t.Fatal("a revoked session's token stayed in the service map")
	}
	if _, ok := s.consumeSocketToken(token, secureRequest(http.MethodGet, "/wt/ping", nil)); ok {
		t.Fatal("token outlived its revoked session")
	}
}

func TestWebTransportTokensExpireAndCapPerSession(t *testing.T) {
	synctest.Test(t, tokensExpireAndCapPerSession)
}

func tokensExpireAndCapPerSession(t *testing.T) {
	s := quietService(t)
	_, sess, err := s.createSession("subject", "Name", "local")
	if err != nil {
		t.Fatal(err)
	}
	stale := mintForSession(t, s, sess)
	time.Sleep(socketTokenLifetime + time.Second)
	if _, ok := s.consumeSocketToken(stale, secureRequest(http.MethodGet, "/wt/ping", nil)); ok {
		t.Fatal("expired token accepted")
	}

	tokens := make([]string, 0, maxSessionSocketTokens)
	for range maxSessionSocketTokens {
		time.Sleep(time.Second)
		tokens = append(tokens, mintForSession(t, s, sess))
	}
	if len(s.socketTokens) != maxSessionSocketTokens {
		t.Fatalf("session holds %d tokens, want the %d cap", len(s.socketTokens), maxSessionSocketTokens)
	}
	r := secureRequest(http.MethodPost, "/wt/session?target=https://meter.example/wt/ping", nil)
	r = r.WithContext(context.WithValue(r.Context(), principalKey{}, Principal{Subject: sess.subject, session: sess}))
	if _, _, mint := s.MintSocketToken(r, route.WebTransport); mint != SocketMintAtCapacity {
		t.Fatalf("mint at the cap = %d, want SocketMintAtCapacity", mint)
	}
	anonymous := secureRequest(http.MethodPost, "/wt/session?target=https://meter.example/wt/ping", nil)
	if _, _, mint := s.MintSocketToken(anonymous, route.WebTransport); mint != SocketMintNoSession {
		t.Fatalf("mint without a principal = %d, want SocketMintNoSession", mint)
	}
	// Every token the cap protected is still spendable.
	for i, token := range tokens {
		if _, ok := s.consumeSocketToken(token, secureRequest(http.MethodGet, "/wt/ping", nil)); !ok {
			t.Fatalf("token %d refused after a mint hit the cap", i)
		}
	}
	// Consuming them frees the cap, so a client that finishes its dials can mint.
	time.Sleep(time.Second)
	mintForSession(t, s, sess)
	// So does letting them expire unspent.
	for range maxSessionSocketTokens - 1 {
		mintForSession(t, s, sess)
	}
	time.Sleep(socketTokenLifetime)
	mintForSession(t, s, sess)
}

func mintWithGrant(t *testing.T, s *Service, grant string) bool {
	t.Helper()
	minted := false
	next := http.HandlerFunc(func(_ http.ResponseWriter, r *http.Request) {
		_, _, mint := s.MintSocketToken(r, route.WebTransport)
		minted = mint == SocketMintOK
	})
	r := secureRequest(http.MethodPost, "/wt/session?target=https://meter.example/wt/ping", nil)
	r.Header.Set("Authorization", "Bearer "+grant)
	w := httptest.NewRecorder()
	s.Enforce(next, Listener{}).ServeHTTP(w, r)
	if w.Code == http.StatusForbidden {
		t.Fatalf("the grant did not reach the mint at all: HTTP %d", w.Code)
	}
	return minted
}

func TestWebTransportMintRefusesABearerGrant(t *testing.T) {
	s := testService(t)
	_, sess, err := s.createSession("subject", "Name", "local")
	if err != nil {
		t.Fatalf("create session: %v", err)
	}
	grant := grantFor(t, s, sess)

	for i := range maxSessionSocketTokens + 1 {
		if mintWithGrant(t, s, grant) {
			t.Fatalf("mint %d from a bearer grant returned a token", i+1)
		}
	}
	if len(s.socketTokens) != 0 {
		t.Fatalf("a grant parked %d tokens on the login's session", len(s.socketTokens))
	}
	// The starvation the refusal prevents: the browser's own mint still lands.
	if reached, status := wtConnect(t, s, "/wt/ping?token="+mintForSession(t, s, sess)); !reached {
		t.Fatalf("the login could not dial after its own grant minted: HTTP %d", status)
	}
}

func TestWebTransportTokensDieWithAnExpiredSession(t *testing.T) {
	synctest.Test(t, tokensDieWithAnExpiredSession)
}

func tokensDieWithAnExpiredSession(t *testing.T) {
	s := testService(t)
	// Off the sweeper's 30 s grid, so the deadline passes between two sweeps.
	time.Sleep(time.Second)
	_, sess, err := s.createSession("subject", "Name", "local")
	if err != nil {
		t.Fatalf("create session: %v", err)
	}
	time.Sleep(sessionLifetime - 10*time.Second)
	r := secureRequest(http.MethodPost, "/wt/session?target=https://meter.example/wt/ping", nil)
	r = r.WithContext(context.WithValue(r.Context(), principalKey{}, Principal{session: sess}))
	token, expires, _ := s.MintSocketToken(r, route.WebTransport)
	if !expires.Equal(sess.expires) {
		t.Fatalf("ticket expires %v, after its login's %v", expires, sess.expires)
	}
	time.Sleep(11 * time.Second)
	synctest.Wait()
	if sess.ctx.Err() == nil {
		t.Fatal("session never reached its deadline")
	}
	h := sha256.Sum256([]byte(token))
	s.mu.Lock()
	_, listed := s.socketTokens[h]
	s.mu.Unlock()
	if !listed {
		t.Fatal("the token was already swept, so this no longer covers the deadline")
	}
	if _, ok := s.consumeSocketToken(token, secureRequest(http.MethodGet, "/wt/ping", nil)); ok {
		t.Fatal("a token minted before the deadline authenticated after it")
	}
}

func TestWebTransportConnectRefusesCleartext(t *testing.T) {
	s := testService(t)
	_, sess, err := s.createSession("subject", "Name", "local")
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
	if _, ok := s.consumeSocketToken(token, secureRequest(http.MethodGet, "/wt/ping", nil)); !ok {
		t.Fatal("a cleartext CONNECT spent the token it was refused for")
	}
}

func TestSocketTicketTargetsNameOneRouteOnThePublicHostname(t *testing.T) {
	s := testService(t)
	_, sess, err := s.createSession("subject", "Name", "local")
	if err != nil {
		t.Fatal(err)
	}
	for _, tc := range []struct {
		target string
		kind   route.Kind
		want   SocketMint
	}{
		{"https://meter.example/wt/ping", route.WebTransport, SocketMintOK},
		{"https://METER.example:8443/wt/upload", route.WebTransport, SocketMintOK},
		{"https://meter.example/ws/ping", route.WebSocket, SocketMintOK},
		{"https://meter.example/wt/ping", route.WebSocket, SocketMintInvalidTarget},
		{"https://meter.example/ws/ping", route.WebTransport, SocketMintInvalidTarget},
		{"https://other.example/wt/ping", route.WebTransport, SocketMintInvalidTarget},
		{"https://meter.example.evil.example/wt/ping", route.WebTransport, SocketMintInvalidTarget},
		{"http://meter.example/wt/ping", route.WebTransport, SocketMintInvalidTarget},
		{"https://user@meter.example/wt/ping", route.WebTransport, SocketMintInvalidTarget},
		{"https://meter.example/wt/ping?token=x", route.WebTransport, SocketMintInvalidTarget},
		{"https://meter.example/secret", route.WebTransport, SocketMintInvalidTarget},
	} {
		r := secureRequest(http.MethodPost, "/wt/session?target="+url.QueryEscape(tc.target), nil)
		r = r.WithContext(context.WithValue(r.Context(), principalKey{}, Principal{session: sess}))
		if _, _, got := s.MintSocketToken(r, tc.kind); got != tc.want {
			t.Errorf("mint %s for %s = %d, want %d", tc.kind, tc.target, got, tc.want)
		}
	}
}

// A CONNECT spends any ticket it carries, and every credential still answers to the request's origin.
func TestWebTransportConnectCredentials(t *testing.T) {
	s := testService(t)
	raw, sess, err := s.createSession("subject", "Name", "local")
	if err != nil {
		t.Fatal(err)
	}
	native := grantFor(t, s, sess)
	browser, _ := approveBrowser(t, s, raw, sess)
	for _, tc := range []struct {
		name           string
		authorization  []string
		origin         string
		reached, spent bool
	}{
		{"ticket", nil, "", true, true},
		{"native grant beside a ticket", []string{native}, "", true, true},
		{"native grant from another origin", []string{native}, requestingUI, false, true},
		{"browser grant from its origin", []string{browser}, requestingUI, true, true},
		{"browser grant from another origin", []string{browser}, "https://other.example", false, true},
		{"native grant repeated beside a ticket", []string{native, "Bearer invalid"}, "", false, false},
	} {
		t.Run(tc.name, func(t *testing.T) {
			ticket := mintForSession(t, s, sess)
			r := secureRequest(http.MethodGet, "/wt/ping?token="+ticket, nil)
			r.Method = http.MethodConnect
			r.Header.Set("Origin", tc.origin)
			for i, value := range tc.authorization {
				if i == 0 {
					value = "Bearer " + value
				}
				r.Header.Add("Authorization", value)
			}
			reached := false
			w := httptest.NewRecorder()
			s.Enforce(http.HandlerFunc(func(http.ResponseWriter, *http.Request) { reached = true }),
				Listener{WebTransport: true}).ServeHTTP(w, r)
			if reached != tc.reached || !reached && w.Code != http.StatusForbidden {
				t.Fatalf("reached=%t status=%d, want reached=%t", reached, w.Code, tc.reached)
			}
			_, unspent := s.consumeSocketToken(ticket, secureRequest(http.MethodGet, "/wt/ping", nil))
			if unspent == tc.spent {
				t.Fatalf("ticket unspent=%t after the CONNECT, want spent=%t", unspent, tc.spent)
			}
		})
	}
}
