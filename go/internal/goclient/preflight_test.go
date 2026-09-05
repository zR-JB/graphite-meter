package goclient

import (
	"net/http"
	"net/http/httptest"
	"net/url"
	"strings"
	"testing"

	"github.com/coder/websocket"

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

func TestEndpointWithQueryReplacesGeneratedValues(t *testing.T) {
	got, err := endpointWithQuery("https://meter.example/download?lane=old&keep=yes", url.Values{
		"lane": {"2"}, "bytes": {"1024"},
	})
	if err != nil {
		t.Fatal(err)
	}
	want := "https://meter.example/download?bytes=1024&keep=yes&lane=2"
	if got != want {
		t.Fatalf("endpointWithQuery() = %q, want %q", got, want)
	}
}

func TestSelectTarget(t *testing.T) {
	webTransport := testTransfer("wt-http3", "https://meter:7249", "http3", true)
	webTransport.Transport = wire.TransportWebTransport
	h1 := testTransfer("http1-clear", "http://meter:7246", "http1", false)
	h2 := testTransfer("http2", "https://meter:7248", "http2", true)
	custom := testTransfer("edge-h2", "https://edge.example", "http2", true)
	pf := wire.Preflight{Capabilities: wire.Capabilities{ThroughputTargets: []wire.ThroughputTarget{webTransport, h1, h2, custom}}}
	for _, tc := range []struct{ selection, base, want string }{
		{"auto", "https://meter:7248", "http2"},
		{"auto", "http://meter:7246", "http1-clear"},
		{"https://meter:7248", "http://discovery", "http2"},
		{"https://edge.example", "http://discovery", "edge-h2"},
	} {
		got, err := selectTarget(Config{ThroughputTarget: tc.selection, BaseURL: tc.base, ThroughputTransport: wire.TransportFetchStream}, pf)
		if err != nil || got.ID != tc.want {
			t.Errorf("select %s = %+v, %v", tc.selection, got, err)
		}
	}
	if _, err := selectTarget(Config{ThroughputTarget: "https://missing.example", ThroughputTransport: "auto"}, pf); err == nil {
		t.Fatal("target absent from the catalog was selected")
	}
}

func TestSelectTargetNormalizesDefaultPort(t *testing.T) {
	pf := wire.Preflight{Capabilities: wire.Capabilities{ThroughputTargets: []wire.ThroughputTarget{
		testTransfer("native-h1", "https://meter.example:443", "http1", true),
		testTransfer("native-h2", "https://meter.example:7248", "http2", true),
	}}}
	got, err := selectTarget(Config{ThroughputTarget: "auto", BaseURL: "https://meter.example", ThroughputTransport: "auto"}, pf)
	if err != nil || got.ID != "native-h1" {
		t.Fatalf("automatic default-port target = %+v, %v", got, err)
	}
}

func TestExplicitTargetsNormalizeDefaultPort(t *testing.T) {
	pf := wire.Preflight{Capabilities: wire.Capabilities{ThroughputTargets: []wire.ThroughputTarget{
		testTransfer("native-h1", "https://meter.example:443", "http1", true),
	}}}
	throughput, err := selectTarget(Config{ThroughputTarget: "https://meter.example", ThroughputTransport: "auto"}, pf)
	if err != nil || throughput.ID != "native-h1" {
		t.Fatalf("explicit throughput target = %+v, %v", throughput, err)
	}
	latency, err := selectLatencyTarget(Config{LatencyTarget: "https://meter.example", BaseURL: "http://discovery", LatencyTransport: "auto"}, []wire.LatencyTarget{
		testChannel("native-h1", "https://meter.example:443", true),
	})
	if err != nil || latency.ID != "native-h1" {
		t.Fatalf("explicit latency target = %+v, %v", latency, err)
	}
}

func TestTargetProtocolEvidence(t *testing.T) {
	for protocol, want := range map[string]string{"http1": "http/1.1", "http2": "h2", "http3": "h3"} {
		if got := targetProtocolEvidence(protocol); got != want {
			t.Errorf("targetProtocolEvidence(%q) = %q, want %q", protocol, got, want)
		}
	}
}

func TestSelectLatencyTargetIsIndependentFromThroughputTarget(t *testing.T) {
	targets := []wire.LatencyTarget{
		testChannel("ws-http1-clear", "http://meter:7246", false),
		testChannel("ws-http1-tls", "https://meter:7247", true),
	}
	if auto, err := selectLatencyTarget(Config{LatencyTarget: "auto", BaseURL: "https://meter:7248", LatencyTransport: "auto"}, targets); err == nil || auto != nil {
		t.Fatalf("ambiguous automatic target = %+v, %v", auto, err)
	}
	explicit, err := selectLatencyTarget(Config{LatencyTarget: "http://meter:7246", BaseURL: "http://meter:7246", LatencyTransport: "auto"}, targets)
	if err != nil || explicit.ID != "ws-http1-clear" {
		t.Fatalf("explicit target = %+v, %v", explicit, err)
	}
}

func TestSelectLatencyTargetFindsLaterSameOriginInHybridCatalog(t *testing.T) {
	targets := []wire.LatencyTarget{
		testChannel("ws-http1-clear", "http://meter.example:7246", false),
		testChannel("ws-http1-tls", "https://meter.example:7247", true),
		testChannel("https://meter.example", "https://meter.example", true),
	}
	got, err := selectLatencyTarget(Config{LatencyTarget: "auto", BaseURL: "https://meter.example", LatencyTransport: "auto"}, targets)
	if err != nil || got.ID != "https://meter.example" {
		t.Fatalf("automatic hybrid latency target = %+v, %v", got, err)
	}
}

func TestSelectLatencyTargetNormalizesDefaultPort(t *testing.T) {
	targets := []wire.LatencyTarget{
		testChannel("native-clear", "http://meter.example:7246", false),
		testChannel("proxy", "https://meter.example:443", true),
	}
	got, err := selectLatencyTarget(Config{LatencyTarget: "auto", BaseURL: "https://meter.example", LatencyTransport: "auto"}, targets)
	if err != nil || got.ID != "proxy" {
		t.Fatalf("automatic default-port latency target = %+v, %v", got, err)
	}
}

func testTransfer(id, origin, protocol string, tls bool) wire.ThroughputTarget {
	return wire.ThroughputTarget{ID: id, Origin: origin, Transport: "fetch-stream", Protocol: protocol, TLS: tls, Routes: wire.DefaultThroughputRoutes()}
}

func testChannel(id, origin string, tls bool) wire.LatencyTarget {
	return wire.LatencyTarget{ID: id, Origin: origin, Transport: "websocket", Protocol: "http1", TLS: tls, Routes: wire.DefaultLatencyRoutes()}
}

func attachTestLatencyTarget(r *runner, origin string) {
	c := testChannel("test-ws", origin, false)
	r.latencyTarget = new(c)
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
			_, _ = w.Write([]byte(`{"server":{"name":"srv","host":"h","port":7246},"engineVersion":"1.0","generation":"test","capabilities":{"throughput":[],"latency":[]}}`))
		}))
		defer srv.Close()

		pf, err := getPreflight(t.Context(), srv.Client(), srv.URL)
		if err != nil {
			t.Fatalf("getPreflight() error: %v", err)
		}
		if pf.Server.Name != "srv" {
			t.Errorf("Server = %+v, unexpected", pf.Server)
		}
		if pf.EngineVersion != "1.0" {
			t.Errorf("EngineVersion = %q", pf.EngineVersion)
		}
	})

	t.Run("non-200 status returns formatted error", func(t *testing.T) {
		srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
			w.WriteHeader(http.StatusInternalServerError)
			_, _ = w.Write([]byte("boom"))
		}))
		defer srv.Close()

		_, err := getPreflight(t.Context(), srv.Client(), srv.URL)
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
			_, _ = w.Write([]byte("{not valid json"))
		}))
		defer srv.Close()

		_, err := getPreflight(t.Context(), srv.Client(), srv.URL)
		if err == nil {
			t.Fatal("expected decode error, got nil")
		}
	})
}

func TestVerifyLatencyWebSocketRequiresMatchingProbeReply(t *testing.T) {
	for _, matching := range []bool{false, true} {
		t.Run(map[bool]string{false: "unmatched", true: "matched"}[matching], func(t *testing.T) {
			server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, request *http.Request) {
				conn, err := websocket.Accept(w, request, &websocket.AcceptOptions{CompressionMode: websocket.CompressionDisabled})
				if err != nil {
					return
				}
				defer conn.CloseNow()
				_, message, err := conn.Read(request.Context())
				if err != nil {
					return
				}
				if string(message) != "PING,0" {
					t.Errorf("readiness sent %q", message)
				}
				for _, reply := range []string{"READY", "PONG,0", "PONG,1,0"} {
					if err := conn.Write(request.Context(), websocket.MessageText, []byte(reply)); err != nil {
						return
					}
				}
				if matching {
					_ = conn.Write(request.Context(), websocket.MessageText, []byte("PONG,0,0"))
				}
			}))
			defer server.Close()
			target := testChannel(server.URL, server.URL, false)
			err := verifyLatencyWebSocket(t.Context(), server.Client(), &target)
			if (err == nil) != matching {
				t.Fatalf("readiness error = %v, matching reply = %v", err, matching)
			}
		})
	}
}
