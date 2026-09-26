package auth

import (
	"net/http"
	"net/http/httptest"
	"slices"
	"strings"
	"testing"
)

func TestCORSPreflight(t *testing.T) {
	s := testService(t)
	preflight := func(path, origin, method, headers string, secure bool) *httptest.ResponseRecorder {
		r := secureRequest(http.MethodOptions, path, nil)
		r.Header.Set("Origin", origin)
		r.Header.Set("Access-Control-Request-Method", method)
		r.Header.Set("Access-Control-Request-Headers", headers)
		rr := httptest.NewRecorder()
		s.corsPreflight(rr, r, trust{Secure: secure, Canonical: secure})
		return rr
	}
	rr := preflight("/download", s.origin, http.MethodGet, "authorization,x-csrf-token", true)
	if rr.Code != http.StatusNoContent || rr.Header().Get("Access-Control-Allow-Origin") != s.origin ||
		rr.Header().Get("Access-Control-Allow-Credentials") != "true" ||
		rr.Header().Get("Access-Control-Max-Age") != "7200" ||
		!strings.Contains(rr.Header().Get("Access-Control-Expose-Headers"), "X-Graphite-Upload-Refusal") {
		t.Fatalf("valid preflight code=%d headers=%v", rr.Code, rr.Header())
	}
	for _, rr := range []*httptest.ResponseRecorder{
		preflight("/download", requestingUI, http.MethodGet, "authorization", true),
		preflight("/auth/browser/token", requestingUI, http.MethodPost, "content-type", true),
	} {
		if rr.Code != http.StatusNoContent || rr.Header().Get("Access-Control-Allow-Origin") != requestingUI ||
			rr.Header().Get("Access-Control-Allow-Credentials") != "" {
			t.Fatalf("cookie-free preflight code=%d headers=%v", rr.Code, rr.Header())
		}
	}
	for name, rr := range map[string]*httptest.ResponseRecorder{
		"insecure":                    preflight("/download", s.origin, http.MethodGet, "", false),
		"cleartext origin":            preflight("/download", "http://meter.example", http.MethodGet, "", true),
		"non-canonical origin":        preflight("/download", "https://meter.example:443", http.MethodGet, "", true),
		"another origin's catalogue":  preflight("/servers", "https://evil.example", http.MethodGet, "", true),
		"browser grant for catalogue": preflight("/servers", requestingUI, http.MethodGet, "authorization", true),
		"bearer without authorization": preflight("/download", requestingUI, http.MethodGet, "content-type",
			true),
		"disallowed header": preflight("/download", s.origin, http.MethodGet, "x-evil", true),
		"token exchange by GET": preflight("/auth/browser/token", requestingUI, http.MethodGet, "content-type",
			true),
		"token exchange by any verb": preflight("/auth/browser/token", requestingUI, "", "content-type", true),
	} {
		if rr.Code != http.StatusForbidden || rr.Header().Get("Access-Control-Allow-Origin") != "" {
			t.Errorf("%s: code=%d exposes %q, want a bare 403", name, rr.Code,
				rr.Header().Get("Access-Control-Allow-Origin"))
		}
	}
	methods := []string{http.MethodGet, http.MethodHead, http.MethodPost, http.MethodDelete, http.MethodConnect,
		http.MethodOptions, http.MethodPut, ""}
	for path, permitted := range map[string][]string{
		"/preflight": {http.MethodGet}, "/probe": {http.MethodGet}, "/download": {http.MethodGet},
		"/upload/session": {http.MethodPost}, "/upload": {http.MethodPost}, "/wt/session": {http.MethodPost},
		"/upload/progress": {http.MethodGet, http.MethodDelete}, "/ws/ping": {http.MethodGet},
		"/wt/download": {http.MethodConnect}, "/wt/upload": {http.MethodConnect}, "/wt/ping": {http.MethodConnect},
		"/login": nil, "/download/": nil, "/secret": nil,
	} {
		for _, method := range methods {
			want := http.StatusForbidden
			if slices.Contains(permitted, method) {
				want = http.StatusNoContent
			}
			if got := preflight(path, s.origin, method, "", true).Code; got != want {
				t.Errorf("preflight %s %s = %d, want %d", method, path, got, want)
			}
		}
	}
}
