package auth

import (
	"net/http"
	"net/http/httptest"
	"testing"

	"github.com/zR-JB/graphite-meter/go/internal/testkit"
)

func TestAmbiguousAuthEvidenceCannotReachAuthenticatedHandler(t *testing.T) {
	s := testService(t)
	token, sess, err := s.createSession("operator", "Operator", "local")
	if err != nil {
		t.Fatal(err)
	}
	grant := grantFor(t, s, sess)
	handler := s.Enforce(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		w.WriteHeader(http.StatusNoContent)
	}), Listener{UI: true})
	request := func() *http.Request {
		r := withSessionCookie(secureRequest(http.MethodPost, "/upload", nil), token)
		r.Header.Set("Origin", s.PublicOrigin())
		r.Header.Set("Sec-Fetch-Site", "same-origin")
		r.Header.Set("X-CSRF-Token", sess.csrf)
		return r
	}
	check := func(r *http.Request, want int) *httptest.ResponseRecorder {
		t.Helper()
		w := testkit.Record(handler.ServeHTTP, r)
		if w.Code != want {
			t.Fatalf("status = %d, want %d", w.Code, want)
		}
		return w
	}
	check(request(), http.StatusNoContent)

	// The first copy is always the valid one, so only refusing the repetition itself can pass.
	for name, first := range map[string]string{"Origin": s.PublicOrigin(), "Sec-Fetch-Site": "same-origin",
		"X-CSRF-Token": sess.csrf, "Authorization": "Bearer " + grant} {
		r := request()
		r.Header.Set(name, first)
		r.Header.Add(name, "invalid")
		if w := check(r, http.StatusForbidden); w.Header().Get("Graphite-Meter-Auth") != "" {
			t.Fatalf("repeated %s was answered as a missing login", name)
		}
	}
	r := request()
	r.Header.Set("Authorization", "")
	check(r, http.StatusForbidden)
	r = request()
	r.AddCookie(&http.Cookie{Name: sessionCookie, Value: "invalid"})
	check(r, http.StatusForbidden)

	for repeated, want := range map[string]int{"": http.StatusNoContent,
		"Access-Control-Request-Method": http.StatusForbidden, "Access-Control-Request-Headers": http.StatusForbidden} {
		r := secureRequest(http.MethodOptions, "/upload", nil)
		r.Header.Set("Origin", s.PublicOrigin())
		r.Header.Set("Access-Control-Request-Method", "POST")
		r.Header.Set("Access-Control-Request-Headers", "content-type")
		if repeated != "" {
			r.Header.Add(repeated, r.Header.Get(repeated))
		}
		check(r, want)
	}
}
