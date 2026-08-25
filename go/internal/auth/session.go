package auth

// session.go owns the memory-only session store: tokens are 256-bit CSPRNG
// values kept only as SHA-256 hashes, every store is bounded and swept, and
// deleting a session cancels its context so in-flight requests unwind.

import (
	"context"
	"crypto/rand"
	"crypto/sha256"
	"encoding/base64"
	"errors"
	"net/http"
	"slices"
	"time"
)

const (
	sessionCookie      = "__Host-gm_session"
	csrfCookie         = "__Host-gm_csrf"
	loginCookie        = "__Host-gm_login"
	transactionCookie  = "__Host-gm_oidc"
	maxSessions        = 1024
	maxSubjectSessions = 8
	sessionLifetime    = 8 * time.Hour
)

type session struct {
	hash [32]byte
	// id names this login to the rest of the server. Not the hash: that is the
	// lookup key for the credential itself.
	id                      string
	subject, name, provider string
	expires, created        time.Time
	ctx                     context.Context
	cancel                  context.CancelFunc
	grants                  map[[32]byte]struct{}
	wtTokens                map[[32]byte]struct{}
	csrf                    string
}

func (s *Service) createSession(subject, name, provider string) (string, *session, error) {
	now := s.now()
	expires := now.Add(sessionLifetime)
	raw := randomToken(32)
	h := sha256.Sum256([]byte(raw))
	csrf, id := randomToken(32), randomToken(16)
	ctx, cancel := context.WithDeadline(context.Background(), expires)
	sess := &session{hash: h, id: id, subject: subject, name: name, provider: provider, expires: expires, created: now, ctx: ctx, cancel: cancel, grants: map[[32]byte]struct{}{}, wtTokens: map[[32]byte]struct{}{}, csrf: csrf}
	s.mu.Lock()
	defer s.mu.Unlock()
	s.expireLocked(now)
	var own []*session
	for _, x := range s.sessions {
		if x.subject == subject {
			own = append(own, x)
		}
	}
	slices.SortFunc(own, func(a, b *session) int {
		if a.created.Before(b.created) {
			return -1
		}
		if b.created.Before(a.created) {
			return 1
		}
		return 0
	})
	if len(own) >= maxSubjectSessions {
		s.deleteSessionLocked(own[0])
	}
	if len(s.sessions) >= maxSessions {
		cancel()
		s.counters.capacity.Add(1)
		return "", nil, errors.New("session capacity reached")
	}
	s.sessions[h] = sess
	return raw, sess, nil
}

// deleteSessionLocked revokes a session together with everything derived from
// it: its native-client grants and any pending browser approvals.
func (s *Service) deleteSessionLocked(sess *session) {
	delete(s.sessions, sess.hash)
	for grant := range sess.grants {
		delete(s.grants, grant)
	}
	for token := range sess.wtTokens {
		delete(s.wtTokens, token)
	}
	for challenge, approval := range s.approvals {
		if approval.session == sess {
			delete(s.approvals, challenge)
		}
	}
	sess.cancel()
}

// deleteSubjectSessionsLocked revokes every session belonging to subject, each
// with its grants and pending approvals, and reports how many. This is "sign
// out everywhere": it reaches sessions the caller holds no token for, which a
// single-session logout cannot. Victims are collected first, not mid-range.
func (s *Service) deleteSubjectSessionsLocked(subject string) int {
	var victims []*session
	for _, sess := range s.sessions {
		if sess.subject == subject {
			victims = append(victims, sess)
		}
	}
	for _, sess := range victims {
		s.deleteSessionLocked(sess)
	}
	return len(victims)
}

// revokeSessionHash deletes the session keyed by h unless it is absent or is
// keep. A fresh sign-in calls it to rotate out the session it replaces, grants
// included, so re-authenticating in a browser cannot leave the prior session
// live and unreachable for the rest of its lifetime.
func (s *Service) revokeSessionHash(h [32]byte, keep *session) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if sess, ok := s.sessions[h]; ok && sess != keep {
		s.deleteSessionLocked(sess)
	}
}

func (s *Service) expireLocked(now time.Time) {
	for _, sess := range s.sessions {
		if !now.Before(sess.expires) {
			s.deleteSessionLocked(sess)
		}
	}
}

func (s *Service) sweep(ctx context.Context) {
	// Faster than the shortest thing it reaps: a CONNECT token lives 30 s and
	// occupies its session's cap until swept, so a minute would let dead tokens
	// crowd out live ones for a client that dials often.
	t := time.NewTicker(wtTokenLifetime)
	defer t.Stop()
	for {
		select {
		case <-ctx.Done():
			return
		case <-t.C:
			s.mu.Lock()
			s.expireLocked(s.now())
			s.expireWTTokensLocked(s.now())
			s.mu.Unlock()
		}
	}
}

// randomToken is n CSPRNG bytes, base64url. crypto/rand.Read never returns an
// error; it crashes the program rather than handing back weak bytes.
func randomToken(n int) string {
	b := make([]byte, n)
	_, _ = rand.Read(b)
	return base64.RawURLEncoding.EncodeToString(b)
}

func setSessionCookie(w http.ResponseWriter, name, value string, expires time.Time) {
	http.SetCookie(w, &http.Cookie{Name: name, Value: value, Path: "/", Expires: expires, MaxAge: int(time.Until(expires).Seconds()), Secure: true, HttpOnly: true, SameSite: http.SameSiteStrictMode})
}

// setCSRFCookie is deliberately readable by the client: the SPA mirrors it into
// the X-CSRF-Token header for the double-submit check. HttpOnly is therefore
// omitted. The token is not a bearer secret; the session cookie beside it is
// HttpOnly.
func setCSRFCookie(w http.ResponseWriter, value string, expires time.Time) {
	http.SetCookie(w, &http.Cookie{Name: csrfCookie, Value: value, Path: "/", Expires: expires, MaxAge: int(time.Until(expires).Seconds()), Secure: true, SameSite: http.SameSiteStrictMode})
}

// setTransactionCookie is Lax rather than Strict because the OIDC callback
// arrives as a cross-site navigation from the identity provider.
func setTransactionCookie(w http.ResponseWriter, value string, expires time.Time) {
	http.SetCookie(w, &http.Cookie{Name: transactionCookie, Value: value, Path: "/", Expires: expires, MaxAge: int(time.Until(expires).Seconds()), Secure: true, HttpOnly: true, SameSite: http.SameSiteLaxMode})
}

func clearTransactionCookie(w http.ResponseWriter) {
	http.SetCookie(w, &http.Cookie{Name: transactionCookie, Path: "/", MaxAge: -1, Expires: time.Unix(1, 0), Secure: true, HttpOnly: true, SameSite: http.SameSiteLaxMode})
}

func clearCookie(w http.ResponseWriter, name string) {
	http.SetCookie(w, &http.Cookie{Name: name, Path: "/", MaxAge: -1, Expires: time.Unix(1, 0), Secure: true, HttpOnly: true, SameSite: http.SameSiteStrictMode})
}
