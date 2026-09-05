package transport

import (
	"context"

	"github.com/coder/websocket"
)

// wsBus adapts a *websocket.Conn to MessageBus: one wire message per text frame.
type wsBus struct {
	conn *websocket.Conn
	ctx  context.Context
}

func (b *wsBus) Recv() (string, error) {
	_, data, err := b.conn.Read(b.ctx)
	if err != nil {
		return "", err
	}
	return string(data), nil
}

func (b *wsBus) Send(msg string) error {
	return b.conn.Write(b.ctx, websocket.MessageText, []byte(msg))
}

// NewWebSocketBus binds message I/O to the upgraded connection's lifetime.
func NewWebSocketBus(ctx context.Context, conn *websocket.Conn) MessageBus {
	return &wsBus{conn: conn, ctx: ctx}
}
