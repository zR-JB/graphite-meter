// Package transport adapts HTTP and WebSocket requests to measurement endpoints.
package transport

import (
	"context"
	"errors"
	"io"
	"net/http"
	"net/url"
)

// Proto names the wire protocol a Session is running over.
type Proto string

const (
	ProtoH1           Proto = "http/1.1"
	ProtoH2           Proto = "h2"
	ProtoH3           Proto = "h3"
	ProtoWS           Proto = "websocket"
	ProtoWebTransport Proto = "webtransport"
)

// ErrUnsupported is returned by Session methods that the concrete transport
// does not provide (e.g. OpenDownloadSink on a message-bus session).
var ErrUnsupported = errors.New("transport: operation not supported on this session")

// MessageBus is the message-delimited channel used by api/wire.md.
type MessageBus interface {
	Recv() (string, error)
	Send(msg string) error
	Reliable() bool
}

// Session is a transport-agnostic measurement session.
type Session interface {
	Context() context.Context
	Query() url.Values
	Proto() Proto

	// HTTP exposes the request/response pair to HTTP-only endpoints.
	HTTP() (w http.ResponseWriter, r *http.Request, ok bool)

	// OpenDownloadSink yields the byte sink for generated download data.
	OpenDownloadSink() (io.Writer, error)

	// OpenUploadSource yields the byte source to drain and count.
	OpenUploadSource() (io.Reader, error)

	// Bus yields the control-message channel, when the session has one
	// (websocketSession only).
	Bus() (MessageBus, bool)
}
