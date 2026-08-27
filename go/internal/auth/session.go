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

type session struct {
	hash                    [32]byte
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
		s.counters.capacity.Add(1)
		return "", nil, errors.New("session capacity reached")
	}
	s.sessions[h] = sess
	return raw, sess, nil
}

func (s *Service) deleteSessionLocked(sess *session) {
	delete(s.sessions, sess.hash)
	maps.DeleteFunc(sess.grants, func(grant [32]byte, _ struct{}) bool {
		delete(s.grants, grant)
		return true
	})
	maps.DeleteFunc(sess.wtTokens, func(token [32]byte, _ struct{}) bool {
		delete(s.wtTokens, token)
		return true
	})
	maps.DeleteFunc(s.approvals, func(_ string, approval *cliApproval) bool { return approval.session == sess })
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
	t := time.Tick(wtTokenLifetime)
	for {
		select {
		case <-ctx.Done():
			return
		case <-t:
			s.mu.Lock()
			s.expireLocked(s.now())
			s.expireWTTokensLocked(s.now())
			s.mu.Unlock()
		}
	}
}

func randomToken(n int) string {
	b := make([]byte, n)
	_, _ = rand.Read(b)
	return base64.RawURLEncoding.EncodeToString(b)
}

func setCookie(w http.ResponseWriter, name, value string, expires time.Time, httpOnly bool, sameSite http.SameSite) {
	http.SetCookie(w, &http.Cookie{Name: name, Value: value, Path: "/", Expires: expires, MaxAge: int(time.Until(expires).Seconds()), Secure: true, HttpOnly: httpOnly, SameSite: sameSite})
}

func clearTransactionCookie(w http.ResponseWriter) {
	http.SetCookie(w, &http.Cookie{Name: transactionCookie, Path: "/", MaxAge: -1, Expires: time.Unix(1, 0), Secure: true, HttpOnly: true, SameSite: http.SameSiteLaxMode})
}

func clearCookie(w http.ResponseWriter, name string) {
	http.SetCookie(w, &http.Cookie{Name: name, Path: "/", MaxAge: -1, Expires: time.Unix(1, 0), Secure: true, HttpOnly: true, SameSite: http.SameSiteStrictMode})
}
