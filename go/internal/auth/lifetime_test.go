package auth

import (
	"context"
	"fmt"
	"net/http"
	"net/http/httptest"
	"testing"
	"testing/synctest"
	"time"

	"github.com/zR-JB/graphite-meter/go/internal/route"
)

func TestExpiredCookieIsRefusedByTheRequestItself(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		s := quietService(t)
		raw, sess, err := s.createSession("subject", "Name", "local")
		if err != nil {
			t.Fatal(err)
		}
		read := func() int {
			r := withSessionCookie(secureRequest(http.MethodGet, "/download", nil), raw)
			r.Header.Set("Sec-Fetch-Site", "same-origin")
			w := httptest.NewRecorder()
			s.Enforce(statusHandler(http.StatusNoContent), Listener{}).ServeHTTP(w, r)
			return w.Code
		}
		if code := read(); code != http.StatusNoContent {
			t.Fatalf("live cookie = %d, want 204", code)
		}
		time.Sleep(time.Until(sess.expires))
		if code := read(); code != http.StatusForbidden || s.sessions[sess.hash] != nil {
			t.Fatalf("cookie at its expiry = %d with the session kept=%t, want a 403 that drops it", code,
				s.sessions[sess.hash] != nil)
		}
	})
}

// A delegated grant ends with its login: new requests are refused and work it admitted is cancelled.
func TestGrantEndsWithItsLoginDeadline(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		s := quietService(t)
		raw, sess, err := s.createSession("subject", "Name", "local")
		if err != nil {
			t.Fatal(err)
		}
		grant := nativeGrant(t, s, raw, sess, "deadline-verifier")
		download := func() *http.Request {
			r := secureRequest(http.MethodGet, "/download", nil)
			r.Header.Set("Authorization", "Bearer "+grant)
			return r
		}
		ended := make(chan bool, 1)
		inflight := s.Enforce(http.HandlerFunc(func(_ http.ResponseWriter, r *http.Request) {
			<-r.Context().Done()
			ended <- SessionEnded(r.Context())
		}), Listener{})
		go inflight.ServeHTTP(httptest.NewRecorder(), download())
		time.Sleep(time.Until(sess.expires))
		synctest.Wait()
		select {
		case cause := <-ended:
			if !cause {
				t.Fatal("the login's deadline ended the work without its cause")
			}
		default:
			t.Fatal("work admitted by the grant outlived its login")
		}
		w := httptest.NewRecorder()
		s.Enforce(statusHandler(http.StatusNoContent), Listener{}).ServeHTTP(w, download())
		if w.Code != http.StatusForbidden {
			t.Fatalf("grant after its login's deadline = %d, want 403", w.Code)
		}
	})
}

func TestApprovedExchangeExpiresWithTheApproval(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		s := quietService(t)
		raw, sess, err := s.createSession("subject", "Name", "local")
		if err != nil {
			t.Fatal(err)
		}
		approveNative(t, s, raw, sess, "slow-terminal-verifier")
		time.Sleep(approvalLifetime)
		if w := cliExchange(s, `{"verifier":"slow-terminal-verifier"}`); w.Code != http.StatusAccepted ||
			len(sess.grants) != 0 {
			t.Fatalf("exchange after the approval lifetime = %d with %d grants, want 202 and none", w.Code,
				len(sess.grants))
		}
	})
}

func TestExpiredSessionsReleaseCapacityAtTheNextLogin(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		s := quietService(t)
		for i := range maxSessions {
			if _, _, err := s.createSession(fmt.Sprint("subject-", i), "Name", "local"); err != nil {
				t.Fatal(err)
			}
		}
		if _, _, err := s.createSession("late", "Name", "local"); err == nil {
			t.Fatal("login past the session capacity succeeded")
		}
		time.Sleep(sessionLifetime)
		if _, _, err := s.createSession("late", "Name", "local"); err != nil || len(s.sessions) != 1 {
			t.Fatalf("login after every session expired: %v with %d sessions", err, len(s.sessions))
		}
	})
}

func TestSocketTicketsFreeTheirCapAndNeverOutliveTheLogin(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		s := quietService(t)
		_, sess, err := s.createSession("subject", "Name", "local")
		if err != nil {
			t.Fatal(err)
		}
		mint := func() (time.Time, SocketMint) {
			r := secureRequest(http.MethodPost, "/wt/session?target=https://meter.example/wt/ping", nil)
			r = r.WithContext(context.WithValue(r.Context(), principalKey{}, Principal{session: sess}))
			_, expires, status := s.MintSocketToken(r, route.WebTransport)
			return expires, status
		}
		for range maxSessionSocketTokens {
			if _, status := mint(); status != SocketMintOK {
				t.Fatalf("mint under the cap = %d", status)
			}
		}
		if _, status := mint(); status != SocketMintAtCapacity {
			t.Fatalf("mint at the cap = %d, want SocketMintAtCapacity", status)
		}
		time.Sleep(socketTokenLifetime)
		if _, status := mint(); status != SocketMintOK {
			t.Fatalf("mint after every ticket expired unspent = %d, want SocketMintOK", status)
		}
		time.Sleep(time.Until(sess.expires) - socketTokenLifetime/2)
		if expires, status := mint(); status != SocketMintOK || !expires.Equal(sess.expires) {
			t.Fatalf("ticket near the login's end expires %v (%d), want the login's %v", expires, status,
				sess.expires)
		}
	})
}
