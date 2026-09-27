// Package testkit holds helpers shared by tests that serve real handlers and sockets.
package testkit

import (
	"net/http"
	"net/http/httptest"
	"testing"
	"time"
)

// Record serves r with h and returns the recorded answer.
func Record(h func(http.ResponseWriter, *http.Request), r *http.Request) *httptest.ResponseRecorder {
	rec := httptest.NewRecorder()
	h(rec, r)
	return rec
}

// Eventually polls done on real time until it holds, failing t with what after within.
func Eventually(t testing.TB, within time.Duration, what string, done func() bool) {
	t.Helper()
	for deadline := time.Now().Add(within); !done(); time.Sleep(10 * time.Millisecond) {
		if time.Now().After(deadline) {
			t.Fatalf("not within %v: %s", within, what)
		}
	}
}
