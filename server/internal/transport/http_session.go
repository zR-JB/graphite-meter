package transport

import (
	"context"
	"io"
	"net"
	"net/http"
	"net/url"
	"strings"
)

// httpSession adapts an (http.ResponseWriter, *http.Request) pair to Session.
// It serves h1/h2/h3 request/response endpoints. The streaming sink/source
// methods land in Stages 2–3; for now they report ErrUnsupported.
type httpSession struct {
	w http.ResponseWriter
	r *http.Request
}

// NewHTTPSession wraps an HTTP request/response as a Session.
func NewHTTPSession(w http.ResponseWriter, r *http.Request) Session {
	return &httpSession{w: w, r: r}
}

func (s *httpSession) Context() context.Context { return s.r.Context() }
func (s *httpSession) Query() url.Values        { return s.r.URL.Query() }

func (s *httpSession) Proto() Proto {
	switch s.r.ProtoMajor {
	case 3:
		return ProtoH3
	case 2:
		return ProtoH2
	default:
		return ProtoH1
	}
}

// ClientIP prefers the first hop of X-Forwarded-For (when behind a proxy),
// falling back to the socket remote address.
func (s *httpSession) ClientIP() string {
	if xff := s.r.Header.Get("X-Forwarded-For"); xff != "" {
		if i := strings.IndexByte(xff, ','); i >= 0 {
			return strings.TrimSpace(xff[:i])
		}
		return strings.TrimSpace(xff)
	}
	host, _, err := net.SplitHostPort(s.r.RemoteAddr)
	if err != nil {
		return s.r.RemoteAddr
	}
	return host
}

func (s *httpSession) HTTP() (http.ResponseWriter, *http.Request, bool) {
	return s.w, s.r, true
}

// OpenDownloadSink yields the ResponseWriter as the byte sink plus a flush that
// drains the HTTP write buffer (so streamed bytes reach the client promptly
// instead of pooling). When the writer is not an http.Flusher the flush is a
// no-op. The download endpoint writes slices of the shared RNG block into this
// sink; a WebTransport SendStream satisfies the same seam in Stage 5.
func (s *httpSession) OpenDownloadSink() (io.Writer, FlushFunc, error) {
	flush := func() error { return nil }
	if f, ok := s.w.(http.Flusher); ok {
		flush = func() error { f.Flush(); return nil }
	}
	return s.w, flush, nil
}

// OpenUploadSource yields the request body as the byte source to drain and
// count (the client streams generated incompressible bytes into it). The upload
// endpoint copies it to io.Discard through a pooled scratch buffer — counting,
// never accumulating. A WebTransport RecvStream satisfies the same seam in
// Stage 5.
func (s *httpSession) OpenUploadSource() (io.Reader, error) {
	if s.r.Body == nil {
		return nil, ErrUnsupported
	}
	return s.r.Body, nil
}

func (s *httpSession) Bus() (MessageBus, bool) {
	return nil, false
}
