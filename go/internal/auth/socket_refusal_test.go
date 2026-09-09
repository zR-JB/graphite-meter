package auth

import (
	"context"
	"net/http"
	"net/http/httptest"
	"net/url"
	"testing"
)

func TestSocketTicketIsSpentBeforeDownstreamRefusal(t *testing.T) {
	for _, kind := range []string{"ws", "wt"} {
		t.Run(kind, func(t *testing.T) {
			s := testService(t)
			_, sess, err := s.createSession("subject", "Name", "local")
			if err != nil {
				t.Fatal(err)
			}
			path := "/" + kind + "/ping"
			mint := func() string {
				t.Helper()
				r := secureRequest(http.MethodPost, "/"+kind+"/session?target="+url.QueryEscape("https://meter.example"+path), nil)
				r.Header.Set("Origin", "https://meter.example")
				r = r.WithContext(context.WithValue(t.Context(), principalKey{}, sessionPrincipal(sess, "local", false)))
				minter := s.MintWebSocketSessionToken
				if kind == "wt" {
					minter = s.MintWebTransportSessionToken
				}
				token, _, status := minter(r)
				if status != WTMintOK {
					t.Fatalf("mint status = %v", status)
				}
				return token
			}
			refuse := true
			dispatches := 0
			handler := s.Enforce(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				dispatches++
				if refuse {
					w.WriteHeader(http.StatusServiceUnavailable)
					return
				}
				w.WriteHeader(http.StatusNoContent)
			}), Listener{WebTransport: kind == "wt"})
			dial := func(token string) *httptest.ResponseRecorder {
				r := secureRequest(http.MethodGet, path+"?token="+token, nil)
				r.Header.Set("Origin", "https://meter.example")
				if kind == "wt" {
					r.Method = http.MethodConnect
				}
				w := httptest.NewRecorder()
				handler.ServeHTTP(w, r)
				return w
			}
			token := mint()
			if response := dial(token); response.Code != http.StatusServiceUnavailable {
				t.Fatalf("downstream refusal = %d", response.Code)
			}
			refuse = false
			if response := dial(token); response.Code != http.StatusForbidden || response.Header().Get("Graphite-Meter-Auth") != "required" {
				t.Fatalf("replayed refused ticket = %d, auth = %q", response.Code, response.Header().Get("Graphite-Meter-Auth"))
			}
			if dispatches != 1 {
				t.Fatalf("spent ticket reached downstream handler: %d dispatches", dispatches)
			}
			if response := dial(mint()); response.Code != http.StatusNoContent {
				t.Fatalf("fresh ticket from unchanged session = %d", response.Code)
			}
		})
	}
}
