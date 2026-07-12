package transport

import (
	"context"
	"io"
	"net/http"
	"net/url"
)

// httpSession adapts an (http.ResponseWriter, *http.Request) pair to Session.
// It serves h1/h2/h3 request/response endpoints; Bus reports not-ok since
// plain HTTP has no message channel.
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

func (s *httpSession) HTTP() (http.ResponseWriter, *http.Request, bool) {
	return s.w, s.r, true
}

// OpenDownloadSink yields the ResponseWriter as the byte sink plus a flush that
// drains the HTTP write buffer (so streamed bytes reach the client promptly
// instead of pooling); a no-op when the writer is not an http.Flusher. The
// download endpoint writes slices of the shared RNG block into this sink — a
// WebTransport SendStream will satisfy the same seam (docs/ARCHITECTURE.md#roadmap).
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
// never accumulating. A WebTransport RecvStream will satisfy the same seam
// (docs/ARCHITECTURE.md#roadmap).
func (s *httpSession) OpenUploadSource() (io.Reader, error) {
	if s.r.Body == nil {
		return nil, ErrUnsupported
	}
	return s.r.Body, nil
}

func (s *httpSession) Bus() (MessageBus, bool) {
	return nil, false
}
