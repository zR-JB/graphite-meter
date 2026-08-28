package auth

import (
	"context"
	"net/http"
	"time"
)

func setSessionCookie(w http.ResponseWriter, name, value string, expires time.Time) {
	setHTTPOnlyCookie(w, name, value, expires, http.SameSiteStrictMode)
}

func setTransactionCookie(w http.ResponseWriter, value string, expires time.Time) {
	setHTTPOnlyCookie(w, transactionCookie, value, expires, http.SameSiteLaxMode)
}

func (s *Service) createSessionUntil(subject, name, provider string, expires time.Time) (string, *session, error) {
	raw, sess, err := s.createSession(subject, name, provider)
	if err != nil {
		return "", nil, err
	}
	if expires.After(sess.expires) {
		expires = sess.expires
	}
	sess.cancel()
	// This context represents the session's absolute expiry, independent of the test lifetime that created it.
	sess.ctx, sess.cancel = context.WithDeadline(context.Background(), expires)
	sess.expires = expires
	return raw, sess, nil
}
