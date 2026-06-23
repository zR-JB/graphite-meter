package transport

import (
	"context"
	"io"
	"net/http"
	"net/url"

	"github.com/coder/websocket"
)

// wsBus adapts a *websocket.Conn to MessageBus: one logical wire message per text
// frame (api/wire.md — WS frames are already message-delimited, no length prefix).
// Reliable() is true: WS rides TCP, which retransmits, so packet loss is hidden.
// Measurable loss is the WT-datagram bus (Stage 5).
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

func (b *wsBus) Reliable() bool { return true }

// websocketSession is a Session over a WebSocket bus (/ws/ping). It exposes a
// MessageBus and reports ErrUnsupported for the HTTP / byte-stream seams — a bus
// endpoint never touches those.
type websocketSession struct {
	conn     *websocket.Conn
	ctx      context.Context
	clientIP string
	query    url.Values
}

// NewWebSocketSession wraps an upgraded WebSocket conn as a Session. ctx bounds
// the bus lifetime (the adapter cancels it when the handler returns); clientIP
// and query are captured from the upgrade request before the hijack.
func NewWebSocketSession(ctx context.Context, conn *websocket.Conn, clientIP string, query url.Values) Session {
	return &websocketSession{conn: conn, ctx: ctx, clientIP: clientIP, query: query}
}

func (s *websocketSession) Context() context.Context { return s.ctx }
func (s *websocketSession) Query() url.Values        { return s.query }
func (s *websocketSession) ClientIP() string         { return s.clientIP }
func (s *websocketSession) Proto() Proto             { return ProtoWS }

func (s *websocketSession) HTTP() (http.ResponseWriter, *http.Request, bool) {
	return nil, nil, false
}

func (s *websocketSession) OpenDownloadSink() (io.Writer, FlushFunc, error) {
	return nil, nil, ErrUnsupported
}

func (s *websocketSession) OpenUploadSource() (io.Reader, error) {
	return nil, ErrUnsupported
}

func (s *websocketSession) Bus() (MessageBus, bool) {
	return &wsBus{conn: s.conn, ctx: s.ctx}, true
}
