package server

import (
	"context"
	"encoding/json/v2"
	"net/http"
	"net/url"
	"testing"
	"time"

	"github.com/quic-go/webtransport-go"
	"github.com/zR-JB/graphite-meter/go/internal/route"
	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

// mintWTToken asks /wt/session for one CONNECT token to path as the browser does.
func (s *authenticatedStack) mintWTToken(t *testing.T, path string) string {
	t.Helper()
	req, _ := http.NewRequest(http.MethodPost,
		s.origin+route.WTSession+"?target="+url.QueryEscape(s.h3URL+path), nil)
	req.Header.Set("Origin", s.origin)
	req.Header.Set("X-CSRF-Token", s.csrf.Value)
	req.AddCookie(s.session)
	res, err := s.uiClient.Do(req)
	if err != nil {
		t.Fatal(err)
	}
	defer res.Body.Close()
	if res.StatusCode != http.StatusOK {
		t.Fatalf("mint status=%d, want 200", res.StatusCode)
	}
	var out struct {
		Token string `json:"token"`
	}
	if err := json.UnmarshalRead(res.Body, &out); err != nil {
		t.Fatal(err)
	}
	if out.Token == "" {
		t.Fatal("mint returned an empty token")
	}
	return out.Token
}

// answersPing proves the session reached the ping endpoint rather than merely completing a handshake.
func answersPing(t *testing.T, sess *webtransport.Session) {
	t.Helper()
	ctx, cancel := context.WithTimeout(t.Context(), 5*time.Second)
	defer cancel()
	for ctx.Err() == nil {
		if err := sess.SendDatagram([]byte(wire.EncodePing(0))); err != nil {
			t.Fatalf("probe: %v", err)
		}
		replyCtx, cancelReply := context.WithTimeout(ctx, 500*time.Millisecond)
		reply, err := sess.ReceiveDatagram(replyCtx)
		cancelReply()
		if err != nil {
			continue // an unacknowledged datagram may simply be lost
		}
		if f, err := wire.DecodePong(string(reply)); err == nil && f.ID == 0 {
			return
		}
	}
	t.Fatal("ping bus never answered its probe")
}

// A CONNECT needs a minted token, spent once from the page's origin, or a native grant; signing out ends the session
// the page's login admitted.
func TestWebTransportConnectAuthentication(t *testing.T) {
	t.Parallel()
	s := newAuthenticatedStack(t)
	page := http.Header{"Origin": {s.origin}}
	dial := func(target string, hdr http.Header) (*webtransport.Session, int) {
		t.Helper()
		d := insecureWTTransport()
		t.Cleanup(func() { _ = d.Close() })
		return dialWebTransport(t, d, target, hdr)
	}
	// Enforce is the only origin policy a CONNECT passes through.
	for _, path := range []string{route.WTPing, route.WTDownload, route.WTUpload} {
		target := func() string { return s.h3URL + path + "?token=" + url.QueryEscape(s.mintWTToken(t, path)) }
		if sess, _ := dial(target(), http.Header{"Origin": {"https://attacker.example"}}); sess != nil {
			t.Fatalf("a %s CONNECT carrying a foreign Origin opened a session", path)
		}
		if sess, status := dial(target(), page); sess == nil {
			t.Fatalf("%s CONNECT from the canonical origin = %d", path, status)
		}
	}
	ping := s.h3URL + route.WTPing
	token := ping + "?token=" + url.QueryEscape(s.mintWTToken(t, route.WTPing))
	sess, status := dial(token, page)
	if sess == nil {
		t.Fatalf("minted CONNECT = %d, want a session", status)
	}
	answersPing(t, sess)
	for _, tc := range []struct {
		name, target string
		hdr          http.Header
	}{
		// A captured URL is worthless once its CONNECT has landed.
		{"replayed token", token, page},
		{"no credential", ping, nil},
		{"forged token", ping + "?token=gmw_nonsense", page},
	} {
		if _, status := dial(tc.target, tc.hdr); status != http.StatusForbidden {
			t.Errorf("%s CONNECT = %d, want %d", tc.name, status, http.StatusForbidden)
		}
	}
	granted, status := dial(ping, http.Header{"Authorization": {"Bearer " + s.grant(t)}})
	if granted == nil {
		t.Fatalf("granted CONNECT = %d, want a session", status)
	}
	answersPing(t, granted)
	s.signOut(t)
	select {
	case <-sess.Context().Done():
	case <-time.After(5 * time.Second):
		t.Error("the WebTransport session outlived the authentication session that admitted it")
	}
}
