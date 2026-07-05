package goclient

import (
	"context"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
)

func TestHTTPEndpoint(t *testing.T) {
	cases := []struct {
		base, path, want string
	}{
		{"http://example.com", "/preflight", "http://example.com/preflight"},
		{"http://example.com/", "/preflight", "http://example.com/preflight"},
		{"http://example.com", "preflight", "http://example.com/preflight"},
	}
	for _, c := range cases {
		got, err := httpEndpoint(c.base, c.path)
		if err != nil {
			t.Fatalf("httpEndpoint(%q, %q) error: %v", c.base, c.path, err)
		}
		if got != c.want {
			t.Errorf("httpEndpoint(%q, %q) = %q, want %q", c.base, c.path, got, c.want)
		}
	}
}

func TestWSEndpoint(t *testing.T) {
	cases := []struct {
		name, base, path, want string
	}{
		{"https scheme becomes wss", "https://example.com", "/ws/ping", "wss://example.com/ws/ping"},
		{"http scheme becomes ws", "http://example.com", "/ws/ping", "ws://example.com/ws/ping"},
		{"unrecognized scheme passed through", "ftp://example.com", "/ws/ping", "ftp://example.com/ws/ping"},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			got, err := wsEndpoint(c.base, c.path)
			if err != nil {
				t.Fatalf("wsEndpoint error: %v", err)
			}
			if got != c.want {
				t.Errorf("wsEndpoint(%q, %q) = %q, want %q", c.base, c.path, got, c.want)
			}
		})
	}
}

func TestGetPreflight(t *testing.T) {
	t.Run("decodes valid JSON", func(t *testing.T) {
		srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
			w.Header().Set("Content-Type", "application/json")
			w.Write([]byte(`{"clientIp":"1.2.3.4","server":{"name":"srv","host":"h","port":8765},"preTestPingMs":12.5,"engineVersion":"1.0","protocolNegotiated":"h1"}`))
		}))
		defer srv.Close()

		pf, err := getPreflight(context.Background(), srv.Client(), srv.URL)
		if err != nil {
			t.Fatalf("getPreflight() error: %v", err)
		}
		if pf.ClientIP != "1.2.3.4" {
			t.Errorf("ClientIP = %q, want 1.2.3.4", pf.ClientIP)
		}
		if pf.Server.Name != "srv" || pf.Server.Port != 8765 {
			t.Errorf("Server = %+v, unexpected", pf.Server)
		}
		if pf.PreTestPingMs != 12.5 {
			t.Errorf("PreTestPingMs = %v, want 12.5", pf.PreTestPingMs)
		}
	})

	t.Run("non-200 status returns formatted error", func(t *testing.T) {
		srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
			w.WriteHeader(http.StatusInternalServerError)
			w.Write([]byte("boom"))
		}))
		defer srv.Close()

		_, err := getPreflight(context.Background(), srv.Client(), srv.URL)
		if err == nil {
			t.Fatal("expected error, got nil")
		}
		if !strings.Contains(err.Error(), "500") {
			t.Errorf("error = %q, want it to mention status 500", err.Error())
		}
	})

	t.Run("malformed JSON body propagates decode error", func(t *testing.T) {
		srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
			w.WriteHeader(http.StatusOK)
			w.Write([]byte("{not valid json"))
		}))
		defer srv.Close()

		_, err := getPreflight(context.Background(), srv.Client(), srv.URL)
		if err == nil {
			t.Fatal("expected decode error, got nil")
		}
	})
}
