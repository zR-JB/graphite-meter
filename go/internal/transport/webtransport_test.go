package transport

import (
	"context"
	"errors"
	"io"
	"testing"
)

// fakeDatagramConn replays queued datagrams and records what was sent.
type fakeDatagramConn struct {
	incoming []string
	sent     []string
	sendErr  error
}

func (c *fakeDatagramConn) SendDatagram(b []byte) error {
	if c.sendErr != nil {
		return c.sendErr
	}
	c.sent = append(c.sent, string(b))
	return nil
}

func (c *fakeDatagramConn) ReceiveDatagram(context.Context) ([]byte, error) {
	if len(c.incoming) == 0 {
		return nil, io.EOF
	}
	next := c.incoming[0]
	c.incoming = c.incoming[1:]
	return []byte(next), nil
}

func TestWebTransportBus(t *testing.T) {
	conn := &fakeDatagramConn{incoming: []string{"PING,1"}}
	bus := NewWebTransportBus(t.Context(), conn)

	msg, err := bus.Recv()
	if err != nil {
		t.Fatalf("Recv: %v", err)
	}
	if msg != "PING,1" {
		t.Errorf("Recv = %q, want PING,1", msg)
	}
	if err := bus.Send("PONG,1;TIME,7"); err != nil {
		t.Fatalf("Send: %v", err)
	}
	if len(conn.sent) != 1 || conn.sent[0] != "PONG,1;TIME,7" {
		t.Errorf("sent = %v, want [PONG,1;TIME,7]", conn.sent)
	}
	if _, err := bus.Recv(); !errors.Is(err, io.EOF) {
		t.Errorf("Recv after drain = %v, want EOF", err)
	}
}
