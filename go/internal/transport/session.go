// Package transport defines the transport-agnostic Session abstraction that
// measurement endpoints are written against. Two Session implementations exist
// today: httpSession (h1/h2/h3 request/response) and websocketSession (the ws
// message bus, websocket_session.go). A WebTransport session is future work —
// see docs/ARCHITECTURE.md#roadmap. An Endpoint's logic is written once and
// runs over whichever Session it is handed.
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
// does not provide (e.g. OpenDownloadSink on a plain preflight request).
var ErrUnsupported = errors.New("transport: operation not supported on this session")

// MessageBus is the message-delimited channel the wire protocol (api/wire.md)
// runs over: a WebSocket connection today, WebTransport datagrams once that
// transport lands (see docs/ARCHITECTURE.md#roadmap). Reliable() reports
// whether the channel retransmits (true for ws/TCP, false for WT datagrams —
// the latter is what makes packet loss measurable).
type MessageBus interface {
	Recv() (string, error)
	Send(msg string) error
	Reliable() bool
}

// FlushFunc flushes any buffered bytes on a download sink (e.g. http.Flusher).
type FlushFunc func() error

// Session is a transport-agnostic measurement session.
type Session interface {
	Context() context.Context
	Query() url.Values
	Proto() Proto

	// HTTP exposes the underlying writer/request for request/response endpoints
	// (preflight, and later the fetch-based download/upload). ok is false for non-HTTP
	// sessions (WebTransport/WebSocket).
	HTTP() (w http.ResponseWriter, r *http.Request, ok bool)

	// OpenDownloadSink yields the byte sink to stream generated data into:
	// the ResponseWriter for HTTP, a uni SendStream once WebTransport exists.
	OpenDownloadSink() (io.Writer, FlushFunc, error)

	// OpenUploadSource yields the byte source to drain and count: r.Body for
	// HTTP, a uni RecvStream once WebTransport exists.
	OpenUploadSource() (io.Reader, error)

	// Bus yields the control-message channel, when the session has one
	// (websocketSession only, today).
	Bus() (MessageBus, bool)
}
