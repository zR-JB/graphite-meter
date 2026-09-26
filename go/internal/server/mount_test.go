package server

import (
	"context"
	"errors"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
	"time"

	"github.com/coder/websocket"
	"github.com/zR-JB/graphite-meter/go/internal/auth"
	"github.com/zR-JB/graphite-meter/go/internal/config"
	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

// publicAuth is the authentication-off service a public listener mounts.
func publicAuth(t testing.TB) *auth.Service {
	t.Helper()
	authn, err := auth.New(t.Context(), config.AuthConfig{Mode: "off"}, nil, false)
	if err != nil {
		t.Fatal(err)
	}
	return authn
}

func testEndpoints(t testing.TB) *endpoints {
	t.Helper()
	cfg := config.Default()
	return buildEndpoints(t.Context(), &cfg)
}

// publicMux mounts e for one public listener topology.
func publicMux(t testing.TB, e *endpoints, topo muxTopology, spa http.Handler) http.Handler {
	t.Helper()
	return newMux(t.Context(), e, topo, spa, publicAuth(t))
}

// A route answers only the methods it publishes: any other is refused before admission or its handler.
func TestMeasurementRoutesDispatchOnlyTheirMethods(t *testing.T) {
	e := testEndpoints(t)
	mux := publicMux(t, e, muxTopology{transfers: true}, nil)
	for _, tc := range []struct{ method, path, allow string }{
		{http.MethodPost, "/download?bytes=1048576", "GET, HEAD, OPTIONS"},
		{http.MethodGet, "/upload?id=x", "OPTIONS, POST"},
		{http.MethodPut, "/upload/progress?id=x", "DELETE, GET, HEAD, OPTIONS"},
	} {
		rec := httptest.NewRecorder()
		mux.ServeHTTP(rec, httptest.NewRequest(tc.method, tc.path, strings.NewReader("not an upload")))
		if rec.Code != http.StatusMethodNotAllowed || rec.Header().Get("Allow") != tc.allow {
			t.Errorf("%s %s = %d Allow %q, want 405 Allow %q", tc.method, tc.path, rec.Code, rec.Header().Get("Allow"),
				tc.allow)
		}
	}
	if requests, _ := e.admission.stats(); requests.peak != 0 {
		t.Fatalf("a refused method reached admission: peak %d", requests.peak)
	}
}

// Public mode holds no session state, so every measurement response, refusal and preflight is open to every origin.
func TestPublicMeasurementCORS(t *testing.T) {
	mux := publicMux(t, testEndpoints(t), muxTopology{discovery: true, transfers: true}, nil)
	for _, method := range []string{http.MethodPost, http.MethodOptions} {
		rec := httptest.NewRecorder()
		req := httptest.NewRequest(method, "/upload?id=forged", nil)
		req.Header.Set("Origin", "https://page.example")
		req.Header.Set("Access-Control-Request-Method", http.MethodPost)
		mux.ServeHTTP(rec, req)
		want := map[string]string{"Access-Control-Allow-Origin": "*", "Timing-Allow-Origin": "*",
			"Access-Control-Expose-Headers": "X-Graphite-Upload-Refusal, Retry-After",
			"X-Graphite-Upload-Refusal":     "invalid"}
		if method == http.MethodOptions {
			want = map[string]string{"Access-Control-Allow-Origin": "*", "Access-Control-Max-Age": "7200",
				"Access-Control-Allow-Methods": "GET, POST, DELETE, OPTIONS", "Access-Control-Allow-Headers": "*"}
			if rec.Code != http.StatusNoContent || rec.Body.Len() != 0 {
				t.Errorf("preflight = %d with %d body bytes, want an empty 204", rec.Code, rec.Body.Len())
			}
		}
		for name, value := range want {
			if got := rec.Header().Get(name); got != value {
				t.Errorf("%s %s = %q, want %q", method, name, got, value)
			}
		}
	}
}

func dialPing(t *testing.T, srv *httptest.Server) *websocket.Conn {
	t.Helper()
	ctx, cancel := context.WithTimeout(t.Context(), 5*time.Second)
	defer cancel()
	conn, _, err := websocket.Dial(ctx, "ws"+strings.TrimPrefix(srv.URL, "http")+"/ws/ping", nil)
	if err != nil {
		t.Fatalf("dial: %v", err)
	}
	t.Cleanup(func() { conn.CloseNow() })
	return conn
}

func TestWebSocketPingEchoesProbes(t *testing.T) {
	t.Parallel()
	srv := httptest.NewServer(publicMux(t, testEndpoints(t), muxTopology{latency: true}, nil))
	defer srv.Close()
	ctx, cancel := context.WithTimeout(t.Context(), 5*time.Second)
	defer cancel()
	conn, res, err := websocket.Dial(ctx, "ws"+strings.TrimPrefix(srv.URL, "http")+"/ws/ping", &websocket.DialOptions{
		CompressionMode: websocket.CompressionNoContextTakeover,
	})
	if err != nil {
		t.Fatalf("dial: %v", err)
	}
	defer conn.CloseNow()
	if got := res.Header.Get("Sec-WebSocket-Extensions"); got != "" {
		t.Fatalf("Sec-WebSocket-Extensions = %q, want no compression negotiation", got)
	}
	send := func(msg string) {
		t.Helper()
		if err := conn.Write(ctx, websocket.MessageText, []byte(msg)); err != nil {
			t.Fatalf("write %q: %v", msg, err)
		}
	}
	recv := func() wire.Pong {
		t.Helper()
		_, data, err := conn.Read(ctx)
		if err != nil {
			t.Fatalf("read: %v", err)
		}
		pong, err := wire.DecodePong(string(data))
		if err != nil {
			t.Fatalf("decode reply %q: %v", data, err)
		}
		return pong
	}
	sent := time.Now()
	send("PING,42")
	if pong := recv(); pong.ID != 42 || pong.HandlingNanos > uint64(time.Since(sent)) {
		t.Fatalf("PING,42 → %+v; want PONG id=42 with a handling time inside the round trip", pong)
	}
	send("PING,4294967295")
	if pong := recv(); pong.ID != 4294967295 {
		t.Fatalf("PING max → %+v; want PONG id=4294967295", pong)
	}
	// Malformed probes are ignored and the bus remains usable.
	send("PNG,5")
	send("PING,5,0")
	send("PING,7")
	if pong := recv(); pong.ID != 7 {
		t.Fatalf("bus did not survive bad frames: PING,7 → %+v", pong)
	}
}

// An oversized frame is a peer forcing the server to buffer.
func TestWebSocketPingRefusesAnOversizedFrame(t *testing.T) {
	t.Parallel()
	srv := httptest.NewServer(publicMux(t, testEndpoints(t), muxTopology{latency: true}, nil))
	defer srv.Close()
	conn := dialPing(t, srv)
	ctx, cancel := context.WithTimeout(t.Context(), 5*time.Second)
	defer cancel()
	if err := conn.Write(ctx, websocket.MessageText, make([]byte, 4096)); err != nil {
		t.Fatalf("write: %v", err)
	}
	if _, _, err := conn.Read(ctx); websocket.CloseStatus(err) != websocket.StatusMessageTooBig {
		t.Fatalf("close = %v, want StatusMessageTooBig: an oversized frame was buffered instead of refused", err)
	}
}

// A hijacked bus outlives http.Server.Shutdown and its request, so the server's context, the request lifetime and
// the idle bound each end it with their own close code.
func TestWebSocketPingReportsWhyItEnded(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		name   string
		status websocket.StatusCode
		setup  func(e *endpoints)
	}{
		{"shutdown", websocket.StatusGoingAway, func(*endpoints) {}},
		{"lifetime", 4002, func(e *endpoints) {
			e.admission = newRequestAdmission(1, 1, 1, 4, 20*time.Millisecond, time.Hour)
		}},
		{"idle", 4001, func(e *endpoints) { e.idleBound = 40 * time.Millisecond }},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			ctx, stop := context.WithCancel(t.Context())
			defer stop()
			e := testEndpoints(t)
			tc.setup(e)
			srv := httptest.NewServer(newMux(ctx, e, muxTopology{latency: true}, nil, publicAuth(t)))
			defer srv.Close()
			conn := dialPing(t, srv)
			if tc.name == "shutdown" {
				stop()
			}
			readCtx, cancel := context.WithTimeout(t.Context(), 5*time.Second)
			defer cancel()
			_, _, err := conn.Read(readCtx)
			if closed, ok := errors.AsType[websocket.CloseError](err); !ok || closed.Code != tc.status ||
				closed.Reason != tc.name {
				t.Fatalf("close = %v, want %d %q", err, tc.status, tc.name)
			}
		})
	}
}
