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

func (s *httpSession) OpenDownloadSink() (io.Writer, error) {
	return s.w, nil
}

// OpenUploadSource yields the request body for the upload endpoint to count.
func (s *httpSession) OpenUploadSource() (io.Reader, error) {
	if s.r.Body == nil {
		return nil, ErrUnsupported
	}
	return s.r.Body, nil
}

func (s *httpSession) Bus() (MessageBus, bool) {
	return nil, false
}
