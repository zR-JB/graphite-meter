// Package transport defines the transport-agnostic Session abstraction that
// measurement endpoints are written against. One Session implementation exists
// per transport: httpSession (h1/h2/h3 request/response, this stage) and, in
// later stages, a WebTransport session and a WebSocket bus. An Endpoint's logic
// is written once and runs over whichever Session it is handed.
package transport

import (
	"context"
	"errors"
	"io"
	"net"
	"net/http"
	"net/url"
	"strings"
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
// runs over: a WebSocket connection or WebTransport datagrams. Declared now;
// implemented in Stage 4 (ws) and Stage 5 (wt). Reliable() reports whether the
// channel retransmits (true for ws/TCP, false for WT datagrams — the latter is
// what makes packet loss measurable).
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
	ClientIP() string
	Proto() Proto

	// HTTP exposes the underlying writer/request for request/response endpoints
	// (preflight, and later the XHR download/upload). ok is false for non-HTTP
	// sessions (WebTransport/WebSocket).
	HTTP() (w http.ResponseWriter, r *http.Request, ok bool)

	// OpenDownloadSink yields the byte sink to stream generated data into
	// (ResponseWriter for HTTP, a uni SendStream for WebTransport). Stage 2+.
	OpenDownloadSink() (io.Writer, FlushFunc, error)

	// OpenUploadSource yields the byte source to drain and count
	// (r.Body for HTTP, a uni RecvStream for WebTransport). Stage 3+.
	OpenUploadSource() (io.Reader, error)

	// Bus yields the control-message channel, when the session has one. Stage 4+.
	Bus() (MessageBus, bool)
}

// ClientIP resolves the caller's address from a request: the first hop of
// X-Forwarded-For when behind a proxy, else the socket remote address. Shared by
// every Session impl so the rule stays identical across transports.
func ClientIP(r *http.Request) string {
	if xff := r.Header.Get("X-Forwarded-For"); xff != "" {
		if i := strings.IndexByte(xff, ','); i >= 0 {
			return strings.TrimSpace(xff[:i])
		}
		return strings.TrimSpace(xff)
	}
	host, _, err := net.SplitHostPort(r.RemoteAddr)
	if err != nil {
		return r.RemoteAddr
	}
	return host
}
