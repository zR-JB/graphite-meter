package goclient

import (
	"encoding/json/v2"
	"io"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"

	"github.com/coder/websocket"
	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

func ambiguousFetch(extra ...wire.ThroughputTarget) http.HandlerFunc {
	targets := append([]wire.ThroughputTarget{
		testTransfer("one", "http://one.example", "negotiated", false),
		testTransfer("two", "http://two.example", "negotiated", false),
	}, extra...)
	return func(w http.ResponseWriter, _ *http.Request) {
		_ = json.MarshalWrite(w, wire.Preflight{
			Generation:   "test",
			Capabilities: wire.Capabilities{ThroughputTargets: targets},
		})
	}
}

func TestSelectTarget(t *testing.T) {
	t.Parallel()
	webTransport := testTransfer("wt-http3", "https://meter:7249", "http3", true)
	webTransport.Transport = wire.TransportWebTransport
	h1 := testTransfer("http1-clear", "http://meter:7246", "http1", false)
	h2 := testTransfer("http2", "https://meter:7248", "http2", true)
	custom := testTransfer("edge-h2", "https://edge.example", "http2", true)
	pf := wire.Preflight{
		Capabilities: wire.Capabilities{ThroughputTargets: []wire.ThroughputTarget{webTransport, h1, h2, custom}},
	}
	for _, tc := range []struct{ selection, base, want string }{
		{"auto", "https://meter:7248", "http2"},
		{"auto", "http://meter:7246", "http1-clear"},
		{"https://meter:7248", "http://discovery", "http2"},
		{"https://edge.example", "http://discovery", "edge-h2"},
	} {
		got, err := selectTarget(Config{
			ThroughputTarget:    tc.selection,
			BaseURL:             tc.base,
			ThroughputTransport: wire.TransportFetchStream,
		}, pf)
		if err != nil || got.ID != tc.want {
			t.Errorf("select %s = %+v, %v", tc.selection, got, err)
		}
	}
	if _, err := selectTarget(Config{
		ThroughputTarget:    "https://missing.example",
		ThroughputTransport: "auto",
	}, pf); err == nil {
		t.Fatal("target absent from the catalog was selected")
	}
}

func TestSelectTargetNormalizesDefaultPort(t *testing.T) {
	t.Parallel()
	pf := wire.Preflight{Capabilities: wire.Capabilities{ThroughputTargets: []wire.ThroughputTarget{
		testTransfer("native-h1", "https://meter.example:443", "http1", true),
		testTransfer("native-h2", "https://meter.example:7248", "http2", true),
	}}}
	got, err := selectTarget(Config{
		ThroughputTarget:    "auto",
		BaseURL:             "https://meter.example",
		ThroughputTransport: "auto",
	}, pf)
	if err != nil || got.ID != "native-h1" {
		t.Fatalf("automatic default-port target = %+v, %v", got, err)
	}
}

func TestExplicitTargetsNormalizeDefaultPort(t *testing.T) {
	t.Parallel()
	pf := wire.Preflight{Capabilities: wire.Capabilities{ThroughputTargets: []wire.ThroughputTarget{
		testTransfer("native-h1", "https://meter.example:443", "http1", true),
	}}}
	throughput, err := selectTarget(Config{ThroughputTarget: "https://meter.example", ThroughputTransport: "auto"}, pf)
	if err != nil || throughput.ID != "native-h1" {
		t.Fatalf("explicit throughput target = %+v, %v", throughput, err)
	}
	latency, err := selectLatencyTarget(Config{
		LatencyTarget:    "https://meter.example",
		BaseURL:          "http://discovery",
		LatencyTransport: "auto",
	}, []wire.LatencyTarget{
		testChannel("native-h1", "https://meter.example:443", true),
	})
	if err != nil || latency.ID != "native-h1" {
		t.Fatalf("explicit latency target = %+v, %v", latency, err)
	}
}

func TestSelectLatencyTargetIsIndependentFromThroughputTarget(t *testing.T) {
	t.Parallel()
	targets := []wire.LatencyTarget{
		testChannel("ws-http1-clear", "http://meter:7246", false),
		testChannel("ws-http1-tls", "https://meter:7247", true),
	}
	if auto, err := selectLatencyTarget(Config{
		LatencyTarget:    "auto",
		BaseURL:          "https://meter:7248",
		LatencyTransport: "auto",
	}, targets); err == nil || auto != nil {
		t.Fatalf("ambiguous automatic target = %+v, %v", auto, err)
	}
	explicit, err := selectLatencyTarget(Config{
		LatencyTarget:    "http://meter:7246",
		BaseURL:          "http://meter:7246",
		LatencyTransport: "auto",
	}, targets)
	if err != nil || explicit.ID != "ws-http1-clear" {
		t.Fatalf("explicit target = %+v, %v", explicit, err)
	}
}

func TestSelectLatencyTargetFindsLaterSameOriginInHybridCatalog(t *testing.T) {
	t.Parallel()
	targets := []wire.LatencyTarget{
		testChannel("ws-http1-clear", "http://meter.example:7246", false),
		testChannel("ws-http1-tls", "https://meter.example:7247", true),
		testChannel("https://meter.example", "https://meter.example", true),
	}
	got, err := selectLatencyTarget(Config{
		LatencyTarget:    "auto",
		BaseURL:          "https://meter.example",
		LatencyTransport: "auto",
	}, targets)
	if err != nil || got.ID != "https://meter.example" {
		t.Fatalf("automatic hybrid latency target = %+v, %v", got, err)
	}
}

func TestSelectLatencyTargetNormalizesDefaultPort(t *testing.T) {
	t.Parallel()
	targets := []wire.LatencyTarget{
		testChannel("native-clear", "http://meter.example:7246", false),
		testChannel("proxy", "https://meter.example:443", true),
	}
	got, err := selectLatencyTarget(Config{
		LatencyTarget:    "auto",
		BaseURL:          "https://meter.example",
		LatencyTransport: "auto",
	}, targets)
	if err != nil || got.ID != "proxy" {
		t.Fatalf("automatic default-port latency target = %+v, %v", got, err)
	}
}

func testTransfer(id, origin, protocol string, tls bool) wire.ThroughputTarget {
	return wire.ThroughputTarget{
		ID:        id,
		Origin:    origin,
		Transport: "fetch-stream",
		Protocol:  protocol,
		TLS:       tls,
		Routes:    wire.DefaultThroughputRoutes(),
	}
}

func testChannel(id, origin string, tls bool) wire.LatencyTarget {
	return wire.LatencyTarget{
		ID:        id,
		Origin:    origin,
		Transport: "websocket",
		Protocol:  "http1",
		TLS:       tls,
		Routes:    wire.DefaultLatencyRoutes(),
	}
}

func attachTestLatencyTarget(r *runner, origin string) {
	c := testChannel("test-ws", origin, false)
	r.latencyTarget = new(c)
}

func TestGetPreflight(t *testing.T) {
	t.Parallel()
	t.Run("decodes valid JSON", func(t *testing.T) {
		srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
			w.Header().Set("Content-Type", "application/json")
			_, _ = io.WriteString(w, `{"server":{"name":"srv","host":"h","port":7246},"engineVersion":"1.0",`+
				`"generation":"test","capabilities":{"throughput":[],"latency":[]}}`)
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

	t.Run("rejects terminal controls in server identity", func(t *testing.T) {
		for _, name := range []string{`\u001b]52;c;cHduZWQ=\u0007`, `\u009b2J`} {
			srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				_, _ = io.WriteString(w, `{"server":{"name":"`+name+`"},"engineVersion":"1.0",`+
					`"generation":"test","capabilities":{"throughput":[],"latency":[]}}`)
			}))
			_, err := getPreflight(t.Context(), srv.Client(), srv.URL)
			srv.Close()
			if err == nil {
				t.Fatalf("accepted server name %s", name)
			}
		}
		srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
			_, _ = io.WriteString(w, `{"defaultSelection":["self"],"servers":[{"id":"self","url":".",`+
				`"name":"Meter","location":"\u001b]0;owned\u0007"}]}`)
		}))
		defer srv.Close()
		if _, err := getCatalog(t.Context(), Config{BaseURL: srv.URL}); err == nil {
			t.Fatal("accepted a catalogue location with terminal controls")
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
	t.Parallel()
	for _, matching := range []bool{false, true} {
		t.Run(map[bool]string{false: "unmatched", true: "matched"}[matching], func(t *testing.T) {
			server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, request *http.Request) {
				conn, err := websocket.Accept(w, request, &websocket.AcceptOptions{
					CompressionMode: websocket.CompressionDisabled,
				})
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
