package endpoint

import (
	"context"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
	"time"

	"github.com/coder/websocket"
	"github.com/zR-JB/graphite-meter/server/internal/wire"
)

// TestPingEcho drives the full bus path (wsAdapter → websocketSession → Ping) over
// a real WebSocket upgrade: PING echoes the id verbatim, HI is acknowledged with
// READY, and a malformed frame is answered with ERR without dropping the bus.
func TestPingEcho(t *testing.T) {
	reg := NewRegistry()
	reg.RegisterWS("/ws/ping", NewPing())
	mux := http.NewServeMux()
	reg.Mount(mux)

	srv := httptest.NewServer(mux)
	defer srv.Close()

	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()

	wsURL := "ws" + strings.TrimPrefix(srv.URL, "http")
	conn, _, err := websocket.Dial(ctx, wsURL+"/ws/ping", nil)
	if err != nil {
		t.Fatalf("dial: %v", err)
	}
	defer conn.Close(websocket.StatusNormalClosure, "")

	send := func(msg string) {
		t.Helper()
		if err := conn.Write(ctx, websocket.MessageText, []byte(msg)); err != nil {
			t.Fatalf("write %q: %v", msg, err)
		}
	}
	recv := func() wire.Frame {
		t.Helper()
		_, data, err := conn.Read(ctx)
		if err != nil {
			t.Fatalf("read: %v", err)
		}
		f, derr := wire.Decode(string(data))
		if derr != nil {
			t.Fatalf("decode reply %q: %v", string(data), derr)
		}
		return f
	}

	// PING echoes the id verbatim with a monotonic TIME stamp.
	send("PING,42")
	if f := recv(); f.Op != wire.OpPONG || f.ID != 42 {
		t.Fatalf("PING,42 → %+v; want PONG id=42", f)
	}

	// uint32 boundary id round-trips.
	send("PING,4294967295")
	if f := recv(); f.Op != wire.OpPONG || f.ID != 4294967295 {
		t.Fatalf("PING max → %+v; want PONG id=4294967295", f)
	}

	// HI is acknowledged with READY.
	send("HI,ws")
	if f := recv(); f.Op != wire.OpREADY {
		t.Fatalf("HI,ws → %+v; want READY", f)
	}

	// A malformed frame is rejected with ERR, and the bus survives for the next PING.
	send("PNG,5")
	if f := recv(); f.Op != wire.OpERR || f.Code != wire.ErrBadOp {
		t.Fatalf("PNG,5 → %+v; want ERR bad_op", f)
	}
	send("PING,7")
	if f := recv(); f.Op != wire.OpPONG || f.ID != 7 {
		t.Fatalf("bus did not survive bad frame: PING,7 → %+v", f)
	}
}
