package transport

import (
	"context"
	"io"
	"net/http"
	"net/url"
	"time"

	"github.com/quic-go/webtransport-go"
)

// DatagramConn is the datagram half of a WebTransport session.
type DatagramConn interface {
	SendDatagram(b []byte) error
	ReceiveDatagram(ctx context.Context) ([]byte, error)
}

// wtDatagramBus adapts WebTransport datagrams to MessageBus: one wire message
// per datagram. Nothing retransmits here, so a ping that never returns is real
// packet loss rather than a stalled reliable queue.
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

// webtransportSession is one logical request inside a WebTransport session:
// either its datagram bus or a single accepted stream. A session hosts many of
// them, so the seams a given request does not use report ErrUnsupported.
type webtransportSession struct {
	ctx   context.Context
	query url.Values
	bus   MessageBus
	sink  io.Writer
	src   io.Reader
	owner string
}

// NewWebTransportBusSession wraps a session's datagram channel as a bus Session.
func NewWebTransportBusSession(ctx context.Context, conn DatagramConn, query url.Values) Session {
	return &webtransportSession{ctx: ctx, query: query, bus: &wtDatagramBus{conn: conn, ctx: ctx}}
}

// NewWebTransportStreamSession wraps one accepted stream as a byte Session.
// sink serves download, src serves upload, and owner is the client key derived
// from the session's CONNECT request, since a stream carries no request.
func NewWebTransportStreamSession(ctx context.Context, query url.Values, sink io.Writer, src io.Reader, owner string) Session {
	return &webtransportSession{ctx: ctx, query: query, sink: sink, src: src, owner: owner}
}

func (s *webtransportSession) Context() context.Context { return s.ctx }
func (s *webtransportSession) Query() url.Values        { return s.query }
func (s *webtransportSession) Proto() Proto             { return ProtoWebTransport }

func (s *webtransportSession) HTTP() (http.ResponseWriter, *http.Request, bool) {
	return nil, nil, false
}

func (s *webtransportSession) OpenDownloadSink() (io.Writer, error) {
	if s.sink == nil {
		return nil, ErrUnsupported
	}
	return s.sink, nil
}

func (s *webtransportSession) OpenUploadSource() (io.Reader, error) {
	if s.src == nil {
		return nil, ErrUnsupported
	}
	return s.src, nil
}

func (s *webtransportSession) Bus() (MessageBus, bool) {
	if s.bus == nil {
		return nil, false
	}
	return s.bus, true
}

// ClientOwner reports the client key of the session this stream belongs to.
func (s *webtransportSession) ClientOwner() string { return s.owner }

// A blocked WebTransport stream operation observes neither its context nor a
// dead session: cancelling alone leaves the call waiting on a session close
// that may never arrive, so the cancel and a deadline poke always travel
// together. These tie that pair to ctx and return the deregistering stop.

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
		s.CancelWrite(0)
		_ = s.SetWriteDeadline(time.Now())
	})
}

// UnblockReadsOnDone releases a read blocked on an idle peer once ctx ends.
func UnblockReadsOnDone(ctx context.Context, s wtReceiveStream) func() bool {
	return context.AfterFunc(ctx, func() {
		s.CancelRead(0)
		_ = s.SetReadDeadline(time.Now())
	})
}
