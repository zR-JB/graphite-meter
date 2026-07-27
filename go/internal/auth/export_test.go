package auth

// export_test.go hands the external auth_test package the one thing it cannot
// build for itself. Principal.session is an unexported field of an unexported
// type, so nothing outside this package can produce a principal whose LoginID
// is non-empty, and the external package is where a test that imports
// internal/endpoint has to live: endpoint imports auth, so an in-package test
// file importing endpoint is an import cycle.

import (
	"context"
	"net/http"
)

// RequestWithLogin binds a principal carrying loginID as its login to r, the
// way Enforce binds one it authenticated from a session cookie.
func RequestWithLogin(r *http.Request, subject, loginID string) *http.Request {
	p := Principal{Subject: subject, session: &session{id: loginID, subject: subject}}
	return r.WithContext(context.WithValue(r.Context(), principalKey{}, p))
}
