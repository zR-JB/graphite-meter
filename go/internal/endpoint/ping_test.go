package endpoint

import (
	"context"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
	"time"

	"github.com/coder/websocket"
	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

func TestPingEcho(t *testing.T) {
	reg := NewRegistry()
	reg.RegisterWS("/ws/ping", NewPing())
	mux := http.NewServeMux()
	reg.Mount(t.Context(), mux)

	srv := httptest.NewServer(mux)
	defer srv.Close()

	ctx, cancel := context.WithTimeout(t.Context(), 5*time.Second)
	defer cancel()

	wsURL := "ws" + strings.TrimPrefix(srv.URL, "http")
	conn, resp, err := websocket.Dial(ctx, wsURL+"/ws/ping", &websocket.DialOptions{
		CompressionMode: websocket.CompressionNoContextTakeover,
	})
	if err != nil {
		t.Fatalf("dial: %v", err)
	}
	defer conn.Close(websocket.StatusNormalClosure, "")
	if got := resp.Header.Get("Sec-WebSocket-Extensions"); got != "" {
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
		f, derr := wire.DecodePong(string(data))
		if derr != nil {
			t.Fatalf("decode reply %q: %v", string(data), derr)
		}
		return f
	}

	// PING echoes the id verbatim with a handling duration.
	send("PING,42")
	if f := recv(); f.ID != 42 {
		t.Fatalf("PING,42 → %+v; want PONG id=42", f)
	}

	// uint32 boundary id round-trips.
	send("PING,4294967295")
	if f := recv(); f.ID != 4294967295 {
		t.Fatalf("PING max → %+v; want PONG id=4294967295", f)
	}

	// Malformed probes are ignored and the bus remains usable.
	send("PNG,5")
	send("PING,5,0")
	send("PING,7")
	if f := recv(); f.ID != 7 {
		t.Fatalf("bus did not survive bad frame: PING,7 → %+v", f)
	}
}
