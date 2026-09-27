package auth

import (
	"context"
	"crypto/rand"
	"crypto/sha256"
	"encoding/base64"
	"errors"
	"maps"
	"net/http"
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

func uniqueCookie(r *http.Request, name string) *http.Cookie {
	cookies := r.CookiesNamed(name)
	if len(cookies) != 1 {
		return nil
	}
	return cookies[0]
}

type session struct {
	hash                    [32]byte
	id                      string
	subject, name, provider string
	expires, created        time.Time
	ctx                     context.Context
	cancel                  context.CancelFunc
	grants                  map[[32]byte]*grant
	csrf                    string
}

func (s *Service) createSession(subject, name, provider string) (string, *session, error) {
	now := time.Now()
	expires := now.Add(sessionLifetime)
	raw := randomToken(32)
	h := sha256.Sum256([]byte(raw))
	csrf, id := randomToken(32), randomToken(16)
	ctx, cancel := context.WithDeadline(context.Background(), expires)
	sess := &session{hash: h, id: id, subject: subject, name: name, provider: provider, expires: expires,
		created: now, ctx: ctx, cancel: cancel, csrf: csrf,
		grants: map[[32]byte]*grant{}}
	s.mu.Lock()
	defer s.mu.Unlock()
	s.expireLocked(now)
	var oldest *session
	count := 0
	for x := range maps.Values(s.sessions) {
		if x.subject == subject {
			count++
			if oldest == nil || x.created.Before(oldest.created) {
				oldest = x
			}
		}
	}
	if count >= maxSubjectSessions {
		s.deleteSessionLocked(oldest)
	}
	if len(s.sessions) >= maxSessions {
		cancel()
		s.count(countCapacity)
		return "", nil, errors.New("session capacity reached")
	}
	s.sessions[h] = sess
	return raw, sess, nil
}

func (s *Service) deleteSessionLocked(sess *session) {
	delete(s.sessions, sess.hash)
	for g := range maps.Values(sess.grants) {
		s.deleteGrantLocked(g)
	}
	maps.DeleteFunc(s.socketTokens, func(_ [32]byte, t socketToken) bool { return t.principal.session == sess })
	maps.DeleteFunc(s.approvals, func(_ string, a *approval) bool { return a.session == sess })
	sess.cancel()
}

func (s *Service) deleteSubjectSessionsLocked(subject string) int {
	count := 0
	for sess := range maps.Values(s.sessions) {
		if sess.subject == subject {
			s.deleteSessionLocked(sess)
			count++
		}
	}
	return count
}

func (s *Service) revokeSessionHash(h [32]byte, keep *session) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if sess := s.sessions[h]; sess != nil && sess != keep {
		s.deleteSessionLocked(sess)
	}
}

func (s *Service) expireLocked(now time.Time) {
	for sess := range maps.Values(s.sessions) {
		if !now.Before(sess.expires) {
			s.deleteSessionLocked(sess)
		}
	}
}

func (s *Service) sweep(ctx context.Context) {
	t := time.Tick(socketTokenLifetime)
	for {
		select {
		case <-ctx.Done():
			return
		case <-t:
			now := time.Now()
			s.mu.Lock()
			s.expireLocked(now)
			s.expireSocketTokensLocked(now)
			s.mu.Unlock()
		}
	}
}

func randomToken(n int) string {
	b := make([]byte, n)
	_, _ = rand.Read(b)
	return base64.RawURLEncoding.EncodeToString(b)
}

func setCookie(w http.ResponseWriter, name, value string, expires time.Time, sameSite http.SameSite) {
	http.SetCookie(w, &http.Cookie{Name: name, Value: value, Path: "/", Expires: expires,
		MaxAge: int(time.Until(expires).Seconds()), Secure: true, HttpOnly: name != csrfCookie, SameSite: sameSite})
}

func clearCookie(w http.ResponseWriter, name string, sameSite http.SameSite) {
	setCookie(w, name, "", time.Unix(1, 0), sameSite)
}

func issueSessionCookies(w http.ResponseWriter, raw string, sess *session) {
	setCookie(w, sessionCookie, raw, sess.expires, http.SameSiteStrictMode)
	setCookie(w, csrfCookie, sess.csrf, sess.expires, http.SameSiteStrictMode)
	clearCookie(w, loginCookie, http.SameSiteStrictMode)
}
