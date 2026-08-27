package transport

import (
	"context"
	"io"
	"net/http"
	"net/url"

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

// websocketSession is a Session over a WebSocket bus (/ws/ping).
type websocketSession struct {
	conn  *websocket.Conn
	ctx   context.Context
	query url.Values
}

// NewWebSocketSession wraps an upgraded WebSocket conn as a Session.
func NewWebSocketSession(ctx context.Context, conn *websocket.Conn, query url.Values) Session {
	return &websocketSession{conn: conn, ctx: ctx, query: query}
}

func (s *websocketSession) Context() context.Context { return s.ctx }
func (s *websocketSession) Query() url.Values        { return s.query }
func (s *websocketSession) Proto() Proto             { return ProtoWS }

func (s *websocketSession) HTTP() (http.ResponseWriter, *http.Request, bool) {
	return nil, nil, false
}

func (s *websocketSession) OpenDownloadSink() (io.Writer, error) {
	return nil, ErrUnsupported
}

func (s *websocketSession) OpenUploadSource() (io.Reader, error) {
	return nil, ErrUnsupported
}

func (s *websocketSession) Bus() (MessageBus, bool) {
	return &wsBus{conn: s.conn, ctx: s.ctx}, true
}
