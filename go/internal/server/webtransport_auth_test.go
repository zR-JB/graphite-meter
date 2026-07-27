package server

import (
	"context"
	"encoding/json"
	"net/http"
	"net/url"
	"strings"
	"testing"
	"time"

	"github.com/quic-go/webtransport-go"
	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

// A browser CONNECT carries neither cookies nor headers, so the whole
// authenticated WebTransport path rests on a token minted over HTTP and spent
// in the CONNECT URL. These exercise it on the wire, against the listener that
// actually mounts the session routes: a simulated CONNECT sets the very request
// fields (TLS, path, protocol) the boundary reads, so it cannot prove them.

// mintWTToken asks /wt/session for one CONNECT token as the browser does.
func (s *authenticatedStack) mintWTToken(t *testing.T) string {
	t.Helper()
	req, _ := http.NewRequest(http.MethodPost, s.origin+routeWTSession, nil)
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
	if err := json.NewDecoder(res.Body).Decode(&out); err != nil {
		t.Fatal(err)
	}
	if out.Token == "" {
		t.Fatal("mint returned an empty token")
	}
	return out.Token
}

// wtDialer returns a dialer for the stack's HTTP/3 listener.
func (s *authenticatedStack) wtDialer(t *testing.T) *webtransport.Dialer {
	t.Helper()
	d := insecureWTDialer()
	t.Cleanup(func() { _ = d.Close() })
	return d
}

// connectPing dials the ping bus, retrying only while the listener comes up, and
// reports the CONNECT status a refusal answered with.
func (s *authenticatedStack) connectPing(t *testing.T, d *webtransport.Dialer, query string, hdr http.Header) (*webtransport.Session, int) {
	t.Helper()
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	target := s.h3URL + routeWTPing + query
	for {
		res, sess, err := d.Dial(ctx, target, hdr)
		if err == nil {
			t.Cleanup(func() { _ = sess.CloseWithError(0, "") })
			return sess, http.StatusOK
		}
		if res != nil {
			return nil, res.StatusCode
		}
		if ctx.Err() != nil {
			t.Fatalf("dial %s: %v", target, err)
		}
		time.Sleep(20 * time.Millisecond)
	}
}

// answersPing proves the session reached the ping endpoint rather than merely
// completing a handshake.
func answersPing(t *testing.T, sess *webtransport.Session) {
	t.Helper()
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	for ctx.Err() == nil {
		if err := sess.SendDatagram([]byte(wire.Encode(wire.Frame{Op: wire.OpHI, Proto: "wt"}))); err != nil {
			t.Fatalf("hello: %v", err)
		}
		replyCtx, cancelReply := context.WithTimeout(ctx, 500*time.Millisecond)
		reply, err := sess.ReceiveDatagram(replyCtx)
		cancelReply()
		if err != nil {
			continue // an unacknowledged datagram may simply be lost
		}
		if f, err := wire.Decode(string(reply)); err == nil && f.Op == wire.OpREADY {
			return
		}
	}
	t.Fatal("ping bus never answered READY")
}

func TestWebTransportConnectSpendsAMintedTokenOnce(t *testing.T) {
	s := newAuthenticatedStack(t)
	d := s.wtDialer(t)
	token := s.mintWTToken(t)
	query := "?token=" + url.QueryEscape(token)

	sess, status := s.connectPing(t, d, query, nil)
	if status != http.StatusOK {
		t.Fatalf("minted CONNECT status=%d, want a session", status)
	}
	answersPing(t, sess)

	// A captured URL is worthless once its CONNECT has landed.
	if _, status := s.connectPing(t, s.wtDialer(t), query, nil); status != http.StatusForbidden {
		t.Errorf("replayed token status=%d, want %d", status, http.StatusForbidden)
	}
}

func TestWebTransportConnectRefusesWithoutACredential(t *testing.T) {
	s := newAuthenticatedStack(t)
	if _, status := s.connectPing(t, s.wtDialer(t), "", nil); status != http.StatusForbidden {
		t.Errorf("uncredentialed CONNECT status=%d, want %d", status, http.StatusForbidden)
	}
	if _, status := s.connectPing(t, s.wtDialer(t), "?token=gmw_nonsense", nil); status != http.StatusForbidden {
		t.Errorf("forged token status=%d, want %d", status, http.StatusForbidden)
	}
}

func TestWebTransportConnectAcceptsANativeGrant(t *testing.T) {
	s := newAuthenticatedStack(t)
	hdr := http.Header{"Authorization": {"Bearer " + s.grant(t)}}
	sess, status := s.connectPing(t, s.wtDialer(t), "", hdr)
	if status != http.StatusOK {
		t.Fatalf("granted CONNECT status=%d, want a session", status)
	}
	answersPing(t, sess)
}

func TestEndingTheAuthSessionUnwindsALiveWebTransportSession(t *testing.T) {
	s := newAuthenticatedStack(t)
	sess, status := s.connectPing(t, s.wtDialer(t), "?token="+url.QueryEscape(s.mintWTToken(t)), nil)
	if status != http.StatusOK {
		t.Fatalf("minted CONNECT status=%d, want a session", status)
	}
	answersPing(t, sess)

	form := url.Values{"csrf": {s.csrf.Value}}.Encode()
	req, _ := http.NewRequest(http.MethodPost, s.origin+"/auth/logout", strings.NewReader(form))
	req.Header.Set("Content-Type", "application/x-www-form-urlencoded")
	req.Header.Set("Origin", s.origin)
	req.AddCookie(s.session)
	res, err := s.uiClient.Do(req)
	if err != nil {
		t.Fatal(err)
	}
	res.Body.Close()

	select {
	case <-sess.Context().Done():
	case <-time.After(5 * time.Second):
		t.Error("the WebTransport session outlived the authentication session that admitted it")
	}
}
