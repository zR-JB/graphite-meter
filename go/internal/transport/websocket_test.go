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

func TestWebSocketBus(t *testing.T) {
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
		// CloseNow skips the close handshake: the client sends no close frame.
		defer conn.CloseNow()

		bus := NewWebSocketBus(r.Context(), conn)

		msg, err := bus.Recv()
		if err != nil {
			srvErrs <- fmt.Errorf("Recv: %w", err)
			return
		}
		if err := bus.Send("echo:" + msg); err != nil {
			srvErrs <- fmt.Errorf("Send: %w", err)
		}
	})

	srv := httptest.NewServer(handler)
	defer srv.Close()

	ctx, cancel := context.WithTimeout(t.Context(), 5*time.Second)
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
