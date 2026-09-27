package transport

import (
	"context"
	"time"

	"github.com/quic-go/webtransport-go"
)

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
