package transport

import (
	"context"
	"time"

	"github.com/quic-go/webtransport-go"
)

// DatagramConn is the datagram half of a WebTransport session.
type DatagramConn interface {
	SendDatagram(b []byte) error
	ReceiveDatagram(ctx context.Context) ([]byte, error)
}

// wtDatagramBus adapts WebTransport datagrams to MessageBus: one wire message per datagram.
type wtDatagramBus struct {
	conn DatagramConn
	ctx  context.Context
}

func (b *wtDatagramBus) Recv() (string, error) {
	data, err := b.conn.ReceiveDatagram(b.ctx)
	if err != nil {
		return "", err
	}
	return string(data), nil
}

func (b *wtDatagramBus) Send(msg string) error { return b.conn.SendDatagram([]byte(msg)) }

// NewWebTransportBus binds datagram messages to the session's lifetime.
func NewWebTransportBus(ctx context.Context, conn DatagramConn) MessageBus {
	return &wtDatagramBus{conn: conn, ctx: ctx}
}

// A blocked WebTransport stream operation observes neither its context nor a dead session.

type wtSendStream interface {
	CancelWrite(webtransport.StreamErrorCode)
	SetWriteDeadline(time.Time) error
}

type wtReceiveStream interface {
	CancelRead(webtransport.StreamErrorCode)
	SetReadDeadline(time.Time) error
}

// UnblockWritesOnDone releases a write blocked on flow control once ctx ends.
func UnblockWritesOnDone(ctx context.Context, s wtSendStream) func() bool {
	return context.AfterFunc(ctx, func() {
		_ = s.SetWriteDeadline(time.Now())
		s.CancelWrite(0)
	})
}

// UnblockReadsOnDone releases a read blocked on an idle peer once ctx ends.
func UnblockReadsOnDone(ctx context.Context, s wtReceiveStream) func() bool {
	return context.AfterFunc(ctx, func() {
		_ = s.SetReadDeadline(time.Now())
		s.CancelRead(0)
	})
}
