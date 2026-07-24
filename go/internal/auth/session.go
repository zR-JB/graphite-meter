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
	"sort"
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
	hash                    [32]byte
	subject, name, provider string
	expires, created        time.Time
	ctx                     context.Context
	cancel                  context.CancelFunc
	grants                  map[[32]byte]struct{}
	csrf                    string
}

func (s *Service) createSession(subject, name, provider string, expires time.Time) (string, *session, error) {
	now := s.now()
	latest := now.Add(sessionLifetime)
	if expires.IsZero() || expires.After(latest) {
		expires = latest
	}
	raw, err := randomToken(32)
	if err != nil {
		return "", nil, err
	}
	h := sha256.Sum256([]byte(raw))
	csrf, err := randomToken(32)
	if err != nil {
		return "", nil, err
	}
	ctx, cancel := context.WithDeadline(context.Background(), expires)
	sess := &session{hash: h, subject: subject, name: name, provider: provider, expires: expires, created: now, ctx: ctx, cancel: cancel, grants: map[[32]byte]struct{}{}, csrf: csrf}
	s.mu.Lock()
	defer s.mu.Unlock()
	s.expireLocked(now)
	var own []*session
	for _, x := range s.sessions {
		if x.subject == subject {
			own = append(own, x)
		}
	}
	sort.Slice(own, func(i, j int) bool { return own[i].created.Before(own[j].created) })
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
	for challenge, approval := range s.approvals {
		if approval.session == sess {
			delete(s.approvals, challenge)
		}
	}
	sess.cancel()
}

// deleteSubjectSessionsLocked revokes every session belonging to subject, each
// with its grants and pending approvals, and returns how many were revoked.
// This is "sign out everywhere": it reaches sessions the caller no longer holds
// a token for — including one a re-login orphaned — which a single-session
// logout cannot. Victims are collected before deletion so the map is not
// mutated mid-range.
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
// keep. A fresh sign-in calls it to rotate out the session it replaces —
// including that session's grants — so re-authenticating in a browser cannot
// leave the prior session live and unreachable for the rest of its lifetime.
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
	t := time.NewTicker(time.Minute)
	defer t.Stop()
	for {
		select {
		case <-ctx.Done():
			return
		case <-t.C:
			s.mu.Lock()
			s.expireLocked(s.now())
			s.mu.Unlock()
		}
	}
}

func randomToken(n int) (string, error) {
	b := make([]byte, n)
	if _, err := rand.Read(b); err != nil {
		return "", err
	}
	return base64.RawURLEncoding.EncodeToString(b), nil
}

func setSessionCookie(w http.ResponseWriter, name, value string, expires time.Time) {
	http.SetCookie(w, &http.Cookie{Name: name, Value: value, Path: "/", Expires: expires, MaxAge: int(time.Until(expires).Seconds()), Secure: true, HttpOnly: true, SameSite: http.SameSiteStrictMode})
}

// setCSRFCookie is deliberately readable by the client: the SPA mirrors it into
// the X-CSRF-Token header for the double-submit check. HttpOnly is therefore
// intentionally omitted — the token is not a bearer secret; the session cookie
// beside it is HttpOnly.
func setCSRFCookie(w http.ResponseWriter, value string, expires time.Time) {
	//nosec G124 -- HttpOnly is intentionally omitted so the SPA can read the CSRF token.
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
