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

func TestTargetSelection(t *testing.T) {
	t.Parallel()
	type throughput = []wire.ThroughputTarget
	type latency = []wire.LatencyTarget
	fetch, wt, ws := wire.TransportFetchStream, wire.TransportWebTransport, wire.TransportWebSocket
	h1 := testTransfer("h1", "http://meter:7246", "http1", false)
	h2 := testTransfer("h2", "https://meter:7248", "http2", true)
	edge := testTransfer("edge", "https://edge.example", "http2", true)
	fetch3 := testTransfer("fetch3", "https://meter:7249", "http3", true)
	wt3 := testTransfer("wt3", "https://meter:7249", "http3", true)
	wt3.Transport = wt
	port443 := testTransfer("443", "https://meter.example:443", "http1", true)
	port7248 := testTransfer("7248", "https://meter.example:7248", "http2", true)
	for _, c := range []struct {
		name, base, target, transport, protocol string
		targets                                 []wire.ThroughputTarget
		want                                    string
	}{
		{"base origin first", "https://meter:7248", "auto", fetch, "auto", throughput{wt3, h1, h2, edge}, "h2"},
		{"explicit origin", "http://discovery", "https://edge.example", fetch, "auto", throughput{h1, edge}, "edge"},
		{"absent origin", "http://discovery", "https://missing.example", "auto", "auto", throughput{h1}, ""},
		{"ambiguous", "http://discovery", "auto", "auto", "auto", throughput{h1, h2}, ""},
		{"protocol narrows", "http://discovery", "auto", "auto", "http2", throughput{h1, h2}, "h2"},
		{"default port", "https://meter.example", "auto", "auto", "auto", throughput{port443, port7248}, "443"},
		{"explicit default port", "http://discovery", "https://meter.example", "auto", "auto",
			throughput{port443}, "443"},
		{"fetch first", "https://meter:7249", "auto", "auto", "auto", throughput{fetch3, wt3}, "fetch3"},
		{"WebTransport alone", "https://meter:7249", "auto", "auto", "auto", throughput{wt3}, "wt3"},
		{"explicit WebTransport", "https://meter:7249", "auto", wt, "auto", throughput{fetch3, wt3}, "wt3"},
		{"no silent fallback", "http://meter:7246", "auto", wt, "auto", throughput{h1}, ""},
	} {
		cfg := Config{BaseURL: c.base, ThroughputTarget: c.target, ThroughputTransport: c.transport,
			ThroughputProtocol: c.protocol}
		got, err := selectTarget(cfg, wire.Preflight{Capabilities: wire.Capabilities{ThroughputTargets: c.targets}})
		if c.want == "" && err == nil || c.want != "" && (err != nil || got.ID != c.want) {
			t.Errorf("throughput %s = %+v, %v; want %q", c.name, got, err, c.want)
		}
	}

	ws1 := testChannel("ws1", "http://meter:7246", false)
	ws2 := testChannel("ws2", "https://meter:7247", true)
	proxy := testChannel("proxy", "https://meter.example:443", true)
	wtPing := testChannel("wtping", "https://meter:7249", true)
	wtPing.Transport, wtPing.Protocol = wt, "http3"
	for _, c := range []struct {
		name, base, target, transport string
		targets                       []wire.LatencyTarget
		want                          string
	}{
		{"ambiguous", "https://meter:7248", "auto", "auto", latency{ws1, ws2}, ""},
		{"explicit origin", "https://meter:7248", "http://meter:7246", "auto", latency{ws1, ws2}, "ws1"},
		{"later base origin", "https://meter.example", "auto", "auto",
			latency{ws1, ws2, testChannel("self", "https://meter.example", true)}, "self"},
		{"default port", "https://meter.example", "auto", "auto",
			latency{testChannel("clear", "http://meter.example:7246", false), proxy}, "proxy"},
		{"explicit default port", "http://discovery", "https://meter.example", "auto", latency{proxy}, "proxy"},
		{"WebTransport first", "https://meter:7249", "auto", "auto", latency{ws2, wtPing}, "wtping"},
		{"explicit WebSocket", "https://meter:7249", "auto", ws, latency{ws2, wtPing}, "ws2"},
		{"no silent fallback", "http://meter:7246", "auto", wt, latency{ws1}, ""},
	} {
		cfg := Config{BaseURL: c.base, LatencyTarget: c.target, LatencyTransport: c.transport}
		got, err := selectLatencyTarget(cfg, c.targets)
		if c.want == "" && err == nil || c.want != "" && (err != nil || got.ID != c.want) {
			t.Errorf("latency %s = %+v, %v; want %q", c.name, got, err, c.want)
		}
	}
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

func TestVerifyLatencyRequiresMatchingProbeReply(t *testing.T) {
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
			rtt, err := verifyLatency(t.Context(), DefaultConfig(), server.Client(), &target)
			if (err == nil) != matching || matching && rtt <= 0 {
				t.Fatalf("readiness = %v, %v; matching reply = %v", rtt, err, matching)
			}
		})
	}
}
