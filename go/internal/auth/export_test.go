package auth

import (
	"context"
	"net/http"
)

func RequestWithLogin(r *http.Request, subject, loginID string) *http.Request {
	p := Principal{Subject: subject, session: &session{id: loginID, subject: subject}}
	return r.WithContext(context.WithValue(r.Context(), principalKey{}, p))
}
