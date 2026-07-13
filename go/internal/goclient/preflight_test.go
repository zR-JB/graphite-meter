package goclient

import (
	"context"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"

	"github.com/zR-JB/graphite-meter/go/internal/wire"
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

func TestSelectTarget(t *testing.T) {
	h1 := testTransfer("http1-clear", "http://meter:8765", "http1", false)
	h2 := testTransfer("http2", "https://meter:8443", "http2", true)
	pf := wire.Preflight{Capabilities: wire.Capabilities{Transfers: []wire.TransferTarget{h1, h2}}}
	for _, tc := range []struct{ protocol, base, want string }{
		{"auto", "http://meter:8765", "http1-clear"}, {"http2", "http://discovery", "http2"},
	} {
		got, err := selectTarget(Config{TransferTarget: tc.protocol, BaseURL: tc.base}, pf)
		if err != nil || got.ID != tc.want {
			t.Errorf("select %s = %+v, %v", tc.protocol, got, err)
		}
	}
	if _, err := selectTarget(Config{TransferTarget: "http3"}, pf); err == nil {
		t.Fatal("unavailable H3 selected")
	}
}

func TestSelectChannelIsIndependentFromTransferTarget(t *testing.T) {
	transfer := testTransfer("http1-tls", "https://meter:8445", "http1", true)
	channels := []wire.ChannelTarget{
		testChannel("ws-http1-tls", transfer.Origin, true),
		testChannel("ws-http3", "https://meter:8444", true),
	}
	auto, err := selectChannel("auto", "latency", &transfer, channels)
	if err != nil || auto.ID != "ws-http1-tls" {
		t.Fatalf("automatic channel = %+v, %v", auto, err)
	}
	explicit, err := selectChannel("ws-http3", "latency", &transfer, channels)
	if err != nil || explicit.ID != "ws-http3" {
		t.Fatalf("explicit channel = %+v, %v", explicit, err)
	}
}

func testTransfer(id, origin, protocol string, tls bool) wire.TransferTarget {
	return wire.TransferTarget{ID: id, Origin: origin, Transport: "fetch-stream", Protocol: protocol, TLS: tls, Routes: wire.DefaultTransferRoutes()}
}

func testChannel(id, origin string, tls bool) wire.ChannelTarget {
	return wire.ChannelTarget{ID: id, Origin: origin, Transport: "websocket", Protocol: "http1", TLS: tls, Routes: wire.DefaultWebSocketRoutes()}
}

func attachTestChannels(r *runner, origin string) {
	c := testChannel("test-ws", origin, false)
	r.latencyChannel, r.progressChannel = &c, &c
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
			w.Write([]byte(`{"server":{"name":"srv","host":"h","port":8765},"engineVersion":"1.0","capabilities":{"transfers":[],"channels":[]}}`))
		}))
		defer srv.Close()

		pf, err := getPreflight(context.Background(), srv.Client(), srv.URL)
		if err != nil {
			t.Fatalf("getPreflight() error: %v", err)
		}
		if pf.Server.Name != "srv" || pf.Server.Port != 8765 {
			t.Errorf("Server = %+v, unexpected", pf.Server)
		}
		if pf.EngineVersion != "1.0" {
			t.Errorf("EngineVersion = %q", pf.EngineVersion)
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
