package auth

import (
	"net/http"
	"net/http/httptest"
	"testing"
)

func TestAmbiguousAuthEvidenceCannotReachAuthenticatedHandler(t *testing.T) {
	s := testService(t)
	token, _, err := s.createSession("operator", "Operator", "local")
	if err != nil {
		t.Fatal(err)
	}
	handler := s.Enforce(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		w.WriteHeader(http.StatusNoContent)
	}), Listener{UI: true})
	request := func() *http.Request {
		r := withSessionCookie(secureRequest(http.MethodGet, "/download", nil), token)
		r.Header.Set("Origin", s.PublicOrigin())
		r.Header.Set("Sec-Fetch-Site", "same-origin")
		return r
	}
	check := func(r *http.Request, want int) {
		t.Helper()
		w := httptest.NewRecorder()
		handler.ServeHTTP(w, r)
		if w.Code != want {
			t.Fatalf("status = %d, want %d", w.Code, want)
		}
	}
	check(request(), http.StatusNoContent)

	for _, name := range []string{"Origin", "Sec-Fetch-Site", "Authorization"} {
		r := request()
		if name == "Authorization" {
			r.Header.Add(name, "")
			r.Header.Add(name, "Bearer invalid")
		} else {
			r.Header.Add(name, r.Header.Get(name))
		}
		check(r, http.StatusForbidden)
	}
	r := request()
	r.Header.Set("Authorization", "")
	check(r, http.StatusForbidden)
	r = request()
	r.AddCookie(&http.Cookie{Name: sessionCookie, Value: "invalid"})
	check(r, http.StatusForbidden)
}

func TestRepeatedPreflightFieldsCannotChooseAnAllowedValue(t *testing.T) {
	s := testService(t)
	handler := s.Enforce(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		w.WriteHeader(http.StatusNoContent)
	}), Listener{UI: true})
	request := func() *http.Request {
		r := secureRequest(http.MethodOptions, "/upload", nil)
		r.Header.Set("Origin", s.PublicOrigin())
		r.Header.Set("Access-Control-Request-Method", "POST")
		r.Header.Set("Access-Control-Request-Headers", "content-type")
		return r
	}
	check := func(r *http.Request, want int) {
		t.Helper()
		w := httptest.NewRecorder()
		handler.ServeHTTP(w, r)
		if w.Code != want {
			t.Fatalf("status = %d, want %d", w.Code, want)
		}
	}
	check(request(), http.StatusNoContent)
	for _, name := range []string{"Origin", "Access-Control-Request-Method", "Access-Control-Request-Headers"} {
		r := request()
		r.Header.Add(name, r.Header.Get(name))
		check(r, http.StatusForbidden)
	}
}
