package auth

import (
	"net/http"
	"time"
)

func setSessionCookie(w http.ResponseWriter, name, value string, expires time.Time) {
	setHTTPOnlyCookie(w, name, value, expires, http.SameSiteStrictMode)
}

func setTransactionCookie(w http.ResponseWriter, value string, expires time.Time) {
	setHTTPOnlyCookie(w, transactionCookie, value, expires, http.SameSiteLaxMode)
}
