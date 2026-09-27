package auth

import (
	"context"
	"crypto/sha256"
	"encoding/json/v2"
	"net/http"
	"net/http/httptest"
	"net/url"
	"strings"
	"testing"
	"testing/synctest"
	"time"

	"github.com/zR-JB/graphite-meter/go/internal/config"
	"github.com/zR-JB/graphite-meter/go/internal/route"
	"github.com/zR-JB/graphite-meter/go/internal/testkit"
)

// mintTicket mints a ticket for path as p's client does, from p's browser origin if it has one.
func mintTicket(t *testing.T, s *Service, p Principal, path string) string {
	t.Helper()
	r := secureRequest(http.MethodPost, "/wt/session?target="+url.QueryEscape("https://meter.example"+path), nil)
	r.Header.Set("Origin", p.browserOrigin())
	spec, _ := route.Lookup(path)
	token, _, status := s.mintSocketToken(r.WithContext(context.WithValue(r.Context(), principalKey{}, p)), spec.Kind)
	if status != http.StatusOK {
		t.Fatalf("mint %s = %d, want a ticket", path, status)
	}
	return token
}

func TestWebTransportTokensDieWithTheirSession(t *testing.T) {
	s := testService(t)
	_, sess, err := s.createSession("subject", "Name", "local")
	if err != nil {
		t.Fatal(err)
	}
	token := mintTicket(t, s, sessionPrincipal(sess, "local", false), "/wt/ping")
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
	synctest.Test(t, func(t *testing.T) {
		s := quietService(t)
		_, sess, err := s.createSession("subject", "Name", "local")
		if err != nil {
			t.Fatal(err)
		}
		login := sessionPrincipal(sess, "local", false)
		stale := mintTicket(t, s, login, "/wt/ping")
		time.Sleep(socketTokenLifetime + time.Second)
		if _, ok := s.consumeSocketToken(stale, secureRequest(http.MethodGet, "/wt/ping", nil)); ok {
			t.Fatal("expired token accepted")
		}

		tokens := make([]string, 0, maxSessionSocketTokens)
		for range maxSessionSocketTokens {
			time.Sleep(time.Second)
			tokens = append(tokens, mintTicket(t, s, login, "/wt/ping"))
		}
		r := secureRequest(http.MethodPost, "/wt/session?target=https://meter.example/wt/ping", nil)
		r = r.WithContext(context.WithValue(r.Context(), principalKey{}, login))
		if _, _, mint := s.mintSocketToken(r, route.WebTransport); mint != http.StatusTooManyRequests {
			t.Fatalf("mint at the cap = %d, want http.StatusTooManyRequests", mint)
		}
		// Every token the cap protected is still spendable.
		for i, token := range tokens {
			if _, ok := s.consumeSocketToken(token, secureRequest(http.MethodGet, "/wt/ping", nil)); !ok {
				t.Fatalf("token %d refused after a mint hit the cap", i)
			}
		}
		// Consuming them frees the cap, so a client that finishes its dials can mint.
		time.Sleep(time.Second)
		mintTicket(t, s, login, "/wt/ping")
		// So does letting them expire unspent.
		for range maxSessionSocketTokens - 1 {
			mintTicket(t, s, login, "/wt/ping")
		}
		time.Sleep(socketTokenLifetime)
		mintTicket(t, s, login, "/wt/ping")
	})
}

func TestWebTransportMintRefusesABearerGrant(t *testing.T) {
	s := testService(t)
	_, sess, err := s.createSession("subject", "Name", "local")
	if err != nil {
		t.Fatalf("create session: %v", err)
	}
	status := 0
	r := secureRequest(http.MethodPost, "/wt/session?target=https://meter.example/wt/ping", nil)
	r.Header.Set("Authorization", "Bearer "+grantFor(t, s, sess))
	s.Enforce(http.HandlerFunc(func(_ http.ResponseWriter, r *http.Request) {
		_, _, status = s.mintSocketToken(r, route.WebTransport)
	}), Listener{}).ServeHTTP(httptest.NewRecorder(), r)
	// A native grant's tickets would spend its login's cap and starve the browser's own dials.
	if status != http.StatusForbidden || len(s.socketTokens) != 0 {
		t.Fatalf("mint from a native grant = %d with %d tokens parked, want 403 and none", status,
			len(s.socketTokens))
	}
}

func TestWebTransportTokensDieWithAnExpiredSession(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
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
		token, expires, _ := s.mintSocketToken(r, route.WebTransport)
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
	})
}

func TestSocketTokenHandler(t *testing.T) {
	public, err := New(t.Context(), config.AuthConfig{Mode: "off"}, nil, false)
	if err != nil {
		t.Fatal(err)
	}
	s := quietService(t)
	_, sess, err := s.createSession("subject", "Name", "local")
	if err != nil {
		t.Fatal(err)
	}
	serve := func(authn *Service, p *Principal) (*httptest.ResponseRecorder, string, int64) {
		r := secureRequest(http.MethodPost, "/wt/session?target=https://meter.example/wt/ping", nil)
		if p != nil {
			r = r.WithContext(context.WithValue(r.Context(), principalKey{}, *p))
		}
		w := testkit.Record(authn.SocketTokenHandler(route.WebTransport).ServeHTTP, r)
		var body struct {
			Token   string `json:"token"`
			Expires int64  `json:"expires"`
		}
		_ = json.Unmarshal(w.Body.Bytes(), &body)
		if w.Header().Get("Graphite-Meter-Auth") != "" {
			t.Fatalf("a ticket answer challenged authentication: %v", w.Header())
		}
		return w, body.Token, body.Expires
	}
	if w, token, expires := serve(public, nil); w.Code != http.StatusOK || token != "" || expires != 0 {
		t.Fatalf("public ticket = %d %q %d", w.Code, token, expires)
	}
	if w, _, _ := serve(s, nil); w.Code != http.StatusForbidden || w.Header().Get("Retry-After") != "" {
		t.Fatalf("anonymous ticket = %d %v", w.Code, w.Header())
	}
	login := &Principal{Subject: sess.subject, session: sess}
	for range maxSessionSocketTokens {
		if w, token, expires := serve(s, login); w.Code != http.StatusOK || !strings.HasPrefix(token, "gmw_") ||
			expires <= time.Now().UnixMilli() {
			t.Fatalf("ticket = %d %q %d", w.Code, token, expires)
		}
	}
	if w, _, _ := serve(s, login); w.Code != http.StatusTooManyRequests || w.Header().Get("Retry-After") != "1" {
		t.Fatalf("ticket at the cap = %d %v", w.Code, w.Header())
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
		want   int
	}{
		{"https://meter.example/wt/ping", route.WebTransport, http.StatusOK},
		{"https://METER.example:8443/wt/upload", route.WebTransport, http.StatusOK},
		{"https://meter.example/ws/ping", route.WebSocket, http.StatusOK},
		{"https://meter.example/wt/ping", route.WebSocket, http.StatusBadRequest},
		{"https://meter.example/ws/ping", route.WebTransport, http.StatusBadRequest},
		{"https://other.example/wt/ping", route.WebTransport, http.StatusBadRequest},
		{"https://meter.example.evil.example/wt/ping", route.WebTransport, http.StatusBadRequest},
		{"http://meter.example/wt/ping", route.WebTransport, http.StatusBadRequest},
		{"https://user@meter.example/wt/ping", route.WebTransport, http.StatusBadRequest},
		{"https://meter.example/wt/ping?token=x", route.WebTransport, http.StatusBadRequest},
		{"https://meter.example/secret", route.WebTransport, http.StatusBadRequest},
	} {
		r := secureRequest(http.MethodPost, "/wt/session?target="+url.QueryEscape(tc.target), nil)
		r = r.WithContext(context.WithValue(r.Context(), principalKey{}, Principal{session: sess}))
		if _, _, got := s.mintSocketToken(r, tc.kind); got != tc.want {
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
	wt := Listener{WebTransport: true}
	for _, tc := range []struct {
		name              string
		authorization     []string
		origin            string
		listener          Listener
		cleartext, cookie bool
		reached, spent    bool
	}{
		{"ticket", nil, "", wt, false, false, true, true},
		{"native grant beside a ticket", []string{native}, "", wt, false, false, true, true},
		{"native grant from another origin", []string{native}, requestingUI, wt, false, false, false, true},
		{"browser grant from its origin", []string{browser}, requestingUI, wt, false, false, true, true},
		{"browser grant from another origin", []string{browser}, "https://other.example", wt, false, false, false,
			true},
		{"native grant repeated beside a ticket", []string{native, "Bearer invalid"}, "", wt, false, false, false,
			false},
		{"ticket on a listener without sessions", nil, "", Listener{}, false, false, false, false},
		{"ticket in cleartext", nil, "", wt, true, false, false, false},
		{"session cookie alone", nil, "", wt, false, true, false, false},
	} {
		t.Run(tc.name, func(t *testing.T) {
			ticket := mintTicket(t, s, sessionPrincipal(sess, "local", false), "/wt/ping")
			r := secureRequest(http.MethodGet, "/wt/ping?token="+ticket, nil)
			if tc.cookie {
				r = withSessionCookie(secureRequest(http.MethodGet, "/wt/ping", nil), raw)
			}
			if tc.cleartext {
				r.TLS = nil
			}
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
				tc.listener).ServeHTTP(w, r)
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
