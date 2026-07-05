package transport

import (
	"context"
	"fmt"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
	"time"

	"github.com/coder/websocket"
)

// TestWebSocketSession drives a websocketSession over a real WebSocket upgrade
// (httptest.NewServer + a real client dial, no mocking of the socket): the
// non-bus seams report their documented values, and the bus round-trips a text
// message end-to-end.
func TestWebSocketSession(t *testing.T) {
	srvErrs := make(chan error, 16)
	srvDone := make(chan struct{})

	handler := http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		defer close(srvDone)
		conn, err := websocket.Accept(w, r, &websocket.AcceptOptions{
			InsecureSkipVerify: true,
			CompressionMode:    websocket.CompressionDisabled,
		})
		if err != nil {
			srvErrs <- fmt.Errorf("accept: %w", err)
			return
		}
		defer conn.CloseNow()

		check := func(cond bool, format string, args ...any) {
			if !cond {
				srvErrs <- fmt.Errorf(format, args...)
			}
		}

		sess := NewWebSocketSession(r.Context(), conn, ClientIP(r), r.URL.Query())

		check(sess.Proto() == ProtoWS, "Proto() = %v, want ProtoWS", sess.Proto())

		if _, _, ok := sess.HTTP(); ok {
			srvErrs <- fmt.Errorf("HTTP() ok = true, want false")
		}

		if _, _, err := sess.OpenDownloadSink(); err != ErrUnsupported {
			srvErrs <- fmt.Errorf("OpenDownloadSink() err = %v, want ErrUnsupported", err)
		}

		if _, err := sess.OpenUploadSource(); err != ErrUnsupported {
			srvErrs <- fmt.Errorf("OpenUploadSource() err = %v, want ErrUnsupported", err)
		}

		bus, ok := sess.Bus()
		check(ok, "Bus() ok = false, want true")
		if bus == nil {
			srvErrs <- fmt.Errorf("Bus() returned a nil bus")
			return
		}
		check(bus.Reliable(), "Reliable() = false, want true")

		msg, err := bus.Recv()
		if err != nil {
			srvErrs <- fmt.Errorf("Recv: %w", err)
			return
		}
		if err := bus.Send("echo:" + msg); err != nil {
			srvErrs <- fmt.Errorf("Send: %w", err)
		}
		// CloseNow (deferred above) skips the graceful close handshake: the
		// client is done reading once it gets the echo, so a graceful Close
		// here would block up to 5s waiting for a close-frame reply it never
		// sends.
	})

	srv := httptest.NewServer(handler)
	defer srv.Close()

	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()

	wsURL := "ws" + strings.TrimPrefix(srv.URL, "http")
	conn, _, err := websocket.Dial(ctx, wsURL, &websocket.DialOptions{
		CompressionMode: websocket.CompressionNoContextTakeover,
	})
	if err != nil {
		t.Fatalf("dial: %v", err)
	}
	defer conn.Close(websocket.StatusNormalClosure, "")

	if err := conn.Write(ctx, websocket.MessageText, []byte("hello")); err != nil {
		t.Fatalf("write: %v", err)
	}
	_, data, err := conn.Read(ctx)
	if err != nil {
		t.Fatalf("read: %v", err)
	}
	if got, want := string(data), "echo:hello"; got != want {
		t.Errorf("round-trip reply = %q, want %q", got, want)
	}

	select {
	case <-srvDone:
	case <-time.After(5 * time.Second):
		t.Fatal("server handler did not finish")
	}
	close(srvErrs)
	for err := range srvErrs {
		t.Error(err)
	}
}
