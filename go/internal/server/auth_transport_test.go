package server

import (
	"context"
	"crypto/rand"
	"crypto/sha256"
	"crypto/tls"
	"encoding/base64"
	"encoding/json/v2"
	"io"
	"net"
	"net/http"
	"net/url"
	"strings"
	"testing"
	"time"

	"github.com/coder/websocket"
	"github.com/quic-go/quic-go/http3"
	"github.com/zR-JB/graphite-meter/go/internal/auth"
	"github.com/zR-JB/graphite-meter/go/internal/transport"
)

// authenticatedStack brings up the three real transports behind one auth service, the way Run wires them.
type authenticatedStack struct {
	authn                        *auth.Service
	origin, h2URL, h3URL         string
	session, csrf                *http.Cookie
	uiClient, h2Client, h3Client *http.Client
}

func newAuthenticatedStack(t *testing.T) *authenticatedStack {
	t.Helper()
	cfg, cm := protocolTestTLS(t)
	// One already-reserved port for the UDP listener and its TCP Alt-Svc companion.
	sockets := newTestListenerSockets(t)
	cfg.Native.H3 = sockets.reserveH3()
	ctx, cancel := context.WithCancel(t.Context())
	t.Cleanup(cancel)

	uiLn, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	h2Ln, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	origin := "https://" + uiLn.Addr().String()
	authn := testPasswordAuth(t, origin)
	e, err := buildEndpoints(ctx, cfg)
	if err != nil {
		t.Fatal(err)
	}

	h1p := &http.Protocols{}
	h1p.SetHTTP1(true)
	uiMux := listenerMuxConfigured(ctx, e, muxTopology{spa: true, discovery: true, latency: true, transfers: true, requiredProto: 1}, http.NotFoundHandler(), authn)
	ui := baseServer(authn.Enforce(uiMux, auth.Listener{UI: true}), h1p)
	go serve(tls.NewListener(uiLn, cm.tlsConfig("http/1.1")), ui)
	t.Cleanup(func() { _ = ui.Close() })

	h2p := &http.Protocols{}
	h2p.SetHTTP2(true)
	h2Mux := listenerMuxConfigured(ctx, e, muxTopology{transfers: true, requiredProto: 2}, http.NotFoundHandler(), authn)
	h2 := baseServer(authn.Enforce(h2Mux, auth.Listener{}), h2p)
	go serve(tls.NewListener(h2Ln, cm.tlsConfig("h2")), h2)
	t.Cleanup(func() { _ = h2.Close() })

	// assembleH3 builds the HTTP/3 listener, rather than a copy of it here.
	build := &listenerBuild{ctx: ctx, cfg: cfg, e: e, authn: authn, cm: cm, sockets: sockets,
		connections: newConnectionAdmission(cfg.MaxConnections, cfg.MaxConnectionsPerClient, cfg.TrustedProxies)}
	if err := build.assembleH3(); err != nil {
		t.Fatal(err)
	}
	for _, svc := range build.services {
		run, stop := svc.run, svc.stop
		go func() { _ = run() }()
		// Service cleanup must still run after t.Context is canceled.
		t.Cleanup(func() { _ = stop(context.Background()) })
	}

	uiProtocols := &http.Protocols{}
	uiProtocols.SetHTTP1(true)
	uiTransport := &http.Transport{TLSClientConfig: &tls.Config{InsecureSkipVerify: true}, Protocols: uiProtocols} //nolint:gosec
	h2Protocols := &http.Protocols{}
	h2Protocols.SetHTTP2(true)
	h2Transport := &http.Transport{TLSClientConfig: &tls.Config{InsecureSkipVerify: true}, Protocols: h2Protocols}                 //nolint:gosec
	h3Transport := &http3.Transport{TLSClientConfig: &tls.Config{InsecureSkipVerify: true}, QUICConfig: transport.NewQUICConfig()} //nolint:gosec
	t.Cleanup(func() {
		uiTransport.CloseIdleConnections()
		h2Transport.CloseIdleConnections()
		_ = h3Transport.Close()
	})

	noRedirect := func(*http.Request, []*http.Request) error { return http.ErrUseLastResponse }
	s := &authenticatedStack{
		authn: authn, origin: origin,
		h2URL:    "https://" + h2Ln.Addr().String(),
		h3URL:    "https://" + cfg.Native.H3,
		uiClient: &http.Client{Transport: uiTransport, CheckRedirect: noRedirect},
		h2Client: &http.Client{Transport: h2Transport, CheckRedirect: noRedirect},
		h3Client: &http.Client{Transport: h3Transport, CheckRedirect: noRedirect},
	}
	s.signIn(t)
	return s
}

// signIn performs the real password login over the UI listener and keeps the session and CSRF cookies it issues.
func (s *authenticatedStack) signIn(t *testing.T) {
	t.Helper()
	page, err := s.uiClient.Get(s.origin + "/login")
	if err != nil {
		t.Fatal(err)
	}
	_, _ = io.Copy(io.Discard, page.Body)
	page.Body.Close()
	if page.StatusCode != http.StatusOK {
		t.Fatalf("login page status=%d", page.StatusCode)
	}
	var formToken *http.Cookie
	for _, c := range page.Cookies() {
		if strings.HasSuffix(c.Name, "gm_login") {
			formToken = c
		}
	}
	if formToken == nil {
		t.Fatal("login page issued no form token")
	}

	form := url.Values{"csrf": {formToken.Value}, "password": {"secret"}}.Encode()
	req, _ := http.NewRequest(http.MethodPost, s.origin+"/auth/password", strings.NewReader(form))
	req.Header.Set("Content-Type", "application/x-www-form-urlencoded")
	req.Header.Set("Origin", s.origin)
	req.AddCookie(formToken)
	res, err := s.uiClient.Do(req)
	if err != nil {
		t.Fatal(err)
	}
	_, _ = io.Copy(io.Discard, res.Body)
	res.Body.Close()
	if res.StatusCode != http.StatusSeeOther {
		t.Fatalf("password login status=%d", res.StatusCode)
	}
	for _, c := range res.Cookies() {
		switch {
		case strings.HasSuffix(c.Name, "gm_session") && c.Value != "":
			s.session = c
		case strings.HasSuffix(c.Name, "gm_csrf") && c.Value != "":
			s.csrf = c
		}
	}
	if s.session == nil || s.csrf == nil {
		t.Fatalf("login did not issue both cookies: session=%v csrf=%v", s.session, s.csrf)
	}
}

// grant walks the native-client approval flow to a bearer token, the same way the TUI does: challenge.
func (s *authenticatedStack) grant(t *testing.T) string {
	t.Helper()
	raw := make([]byte, 32)
	if _, err := rand.Read(raw); err != nil {
		t.Fatal(err)
	}
	verifier := base64.RawURLEncoding.EncodeToString(raw)
	sum := sha256.Sum256([]byte(verifier))
	challenge := base64.RawURLEncoding.EncodeToString(sum[:])

	pageReq, _ := http.NewRequest(http.MethodGet, s.origin+"/auth/cli?challenge="+url.QueryEscape(challenge), nil)
	pageReq.AddCookie(s.session)
	res, err := s.uiClient.Do(pageReq)
	if err != nil {
		t.Fatal(err)
	}
	_, _ = io.Copy(io.Discard, res.Body)
	res.Body.Close()
	if res.StatusCode != http.StatusOK {
		t.Fatalf("approval page status=%d", res.StatusCode)
	}

	form := url.Values{"csrf": {s.csrf.Value}, "challenge": {challenge}}.Encode()
	approve, _ := http.NewRequest(http.MethodPost, s.origin+"/auth/cli/approve", strings.NewReader(form))
	approve.Header.Set("Content-Type", "application/x-www-form-urlencoded")
	approve.Header.Set("Origin", s.origin)
	approve.AddCookie(s.session)
	res, err = s.uiClient.Do(approve)
	if err != nil {
		t.Fatal(err)
	}
	_, _ = io.Copy(io.Discard, res.Body)
	res.Body.Close()
	if res.StatusCode != http.StatusOK {
		t.Fatalf("approval status=%d", res.StatusCode)
	}

	body, _ := json.Marshal(map[string]string{"verifier": verifier})
	exchange, _ := http.NewRequest(http.MethodPost, s.origin+"/auth/cli/token", strings.NewReader(string(body)))
	exchange.Header.Set("Content-Type", "application/json")
	res, err = s.uiClient.Do(exchange)
	if err != nil {
		t.Fatal(err)
	}
	defer res.Body.Close()
	var out struct {
		Token string `json:"token"`
	}
	if err := json.UnmarshalRead(res.Body, &out); err != nil {
		t.Fatal(err)
	}
	if res.StatusCode != http.StatusOK || out.Token == "" {
		t.Fatalf("grant exchange status=%d token=%q", res.StatusCode, out.Token)
	}
	return out.Token
}

// The positive path over the real transports.
func TestAuthenticatedMeasurementSucceedsOverEveryTransport(t *testing.T) {
	s := newAuthenticatedStack(t)
	bearer := s.grant(t)

	for _, tc := range []struct {
		name   string
		client *http.Client
		base   string
	}{
		{"http1-tls", s.uiClient, s.origin},
		{"http2", s.h2Client, s.h2URL},
		{"http3", s.h3Client, s.h3URL},
	} {
		t.Run(tc.name+"/session-cookie", func(t *testing.T) {
			req, _ := http.NewRequest(http.MethodGet, tc.base+"/download?bytes=1", nil)
			req.AddCookie(s.session)
			req.Header.Set("Origin", s.origin)
			res, err := tc.client.Do(req)
			if err != nil {
				t.Fatal(err)
			}
			body, _ := io.ReadAll(res.Body)
			res.Body.Close()
			if res.StatusCode != http.StatusOK || len(body) != 1 {
				t.Fatalf("status=%d bytes=%d, want 200 and 1 byte", res.StatusCode, len(body))
			}
		})

		t.Run(tc.name+"/bearer-grant", func(t *testing.T) {
			req, _ := http.NewRequest(http.MethodGet, tc.base+"/download?bytes=1", nil)
			req.Header.Set("Authorization", "Bearer "+bearer)
			res, err := tc.client.Do(req)
			if err != nil {
				t.Fatal(err)
			}
			body, _ := io.ReadAll(res.Body)
			res.Body.Close()
			if res.StatusCode != http.StatusOK || len(body) != 1 {
				t.Fatalf("status=%d bytes=%d, want 200 and 1 byte", res.StatusCode, len(body))
			}
		})
	}
}

func TestAuthenticatedWebSocketUpgradeSucceeds(t *testing.T) {
	s := newAuthenticatedStack(t)
	bearer := s.grant(t)
	wsURL := "wss" + strings.TrimPrefix(s.origin, "https") + "/ws/ping"

	for _, tc := range []struct {
		name    string
		headers http.Header
	}{
		{"session-cookie", http.Header{"Cookie": {s.session.String()}, "Origin": {s.origin}}},
		{"bearer-grant", http.Header{"Authorization": {"Bearer " + bearer}}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			ctx, cancel := context.WithTimeout(t.Context(), 10*time.Second)
			defer cancel()
			conn, res, err := websocket.Dial(ctx, wsURL, &websocket.DialOptions{HTTPClient: s.uiClient, HTTPHeader: tc.headers})
			if err != nil {
				status := 0
				if res != nil {
					status = res.StatusCode
					res.Body.Close()
				}
				t.Fatalf("authenticated WebSocket upgrade failed: %v (status=%d)", err, status)
			}
			conn.Close(websocket.StatusNormalClosure, "")
		})
	}
}

// A bearer grant authorizes measurement and nothing else: it must not reach the session surface or the approval routes.
func TestBearerGrantIsConfinedToMeasurementRoutes(t *testing.T) {
	s := newAuthenticatedStack(t)
	bearer := s.grant(t)
	for _, path := range []string{"/auth/session", "/auth/cli", "/"} {
		req, _ := http.NewRequest(http.MethodGet, s.origin+path, nil)
		req.Header.Set("Authorization", "Bearer "+bearer)
		res, err := s.uiClient.Do(req)
		if err != nil {
			t.Fatal(err)
		}
		_, _ = io.Copy(io.Discard, res.Body)
		res.Body.Close()
		if res.StatusCode == http.StatusOK {
			t.Fatalf("bearer grant reached %s", path)
		}
	}
}

func TestUnauthenticatedRequestsStillFailOnEveryTransport(t *testing.T) {
	s := newAuthenticatedStack(t)
	for _, tc := range []struct {
		name   string
		client *http.Client
		base   string
	}{
		{"http1-tls", s.uiClient, s.origin},
		{"http2", s.h2Client, s.h2URL},
		{"http3", s.h3Client, s.h3URL},
	} {
		t.Run(tc.name, func(t *testing.T) {
			res, err := tc.client.Get(tc.base + "/download?bytes=1")
			if err != nil {
				t.Fatal(err)
			}
			_, _ = io.Copy(io.Discard, res.Body)
			res.Body.Close()
			if res.StatusCode != http.StatusForbidden {
				t.Fatalf("status=%d, want 403", res.StatusCode)
			}
			if res.Header.Get("Graphite-Meter-Auth") != "required" {
				t.Fatalf("missing auth marker: %v", res.Header)
			}
		})
	}
}
