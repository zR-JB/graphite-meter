package transport

import (
	"bytes"
	"io"
	"net/http"
	"net/http/httptest"
	"testing"
)

// TestHTTPSessionProto covers the ProtoMajor → Proto mapping, including the
// default case for anything other than 2 or 3.
func TestHTTPSessionProto(t *testing.T) {
	tests := []struct {
		protoMajor int
		want       Proto
	}{
		{1, ProtoH1},
		{2, ProtoH2},
		{3, ProtoH3},
		{0, ProtoH1}, // unrecognized major falls back to h1
	}

	for _, tt := range tests {
		r := httptest.NewRequest("GET", "/", nil)
		r.ProtoMajor = tt.protoMajor
		s := NewHTTPSession(httptest.NewRecorder(), r)
		if got := s.Proto(); got != tt.want {
			t.Errorf("ProtoMajor=%d: Proto() = %q, want %q", tt.protoMajor, got, tt.want)
		}
	}
}

// TestHTTPSessionHTTP checks HTTP() reports ok and returns the exact
// writer/request the session was built with.
func TestHTTPSessionHTTP(t *testing.T) {
	r := httptest.NewRequest("GET", "/", nil)
	w := httptest.NewRecorder()
	s := NewHTTPSession(w, r)

	gotW, gotR, ok := s.HTTP()
	if !ok {
		t.Fatal("HTTP() ok = false, want true")
	}
	if gotW != w {
		t.Error("HTTP() returned a different ResponseWriter")
	}
	if gotR != r {
		t.Error("HTTP() returned a different *http.Request")
	}
}

// TestHTTPSessionOpenUploadSourceNilBody checks the documented ErrUnsupported
// when the request has no body.
func TestHTTPSessionOpenUploadSourceNilBody(t *testing.T) {
	r := httptest.NewRequest("POST", "/", nil)
	r.Body = nil
	s := NewHTTPSession(httptest.NewRecorder(), r)

	src, err := s.OpenUploadSource()
	if err != ErrUnsupported {
		t.Errorf("err = %v, want ErrUnsupported", err)
	}
	if src != nil {
		t.Errorf("source = %v, want nil", src)
	}
}

// TestHTTPSessionOpenUploadSourceBody checks a real body is passed through
// verbatim as the byte source.
func TestHTTPSessionOpenUploadSourceBody(t *testing.T) {
	want := []byte("payload bytes")
	r := httptest.NewRequest("POST", "/", bytes.NewReader(want))
	s := NewHTTPSession(httptest.NewRecorder(), r)

	src, err := s.OpenUploadSource()
	if err != nil {
		t.Fatalf("err = %v, want nil", err)
	}
	got, err := io.ReadAll(src)
	if err != nil {
		t.Fatalf("ReadAll: %v", err)
	}
	if !bytes.Equal(got, want) {
		t.Errorf("body = %q, want %q", got, want)
	}
}

// TestHTTPSessionBus checks a request/response session has no bus.
func TestHTTPSessionBus(t *testing.T) {
	s := NewHTTPSession(httptest.NewRecorder(), httptest.NewRequest("GET", "/", nil))
	bus, ok := s.Bus()
	if bus != nil || ok {
		t.Errorf("Bus() = (%v, %v), want (nil, false)", bus, ok)
	}
}

// TestHTTPSessionOpenDownloadSinkFlusher checks the sink is the ResponseWriter
// itself and flush drains the underlying http.Flusher.
func TestHTTPSessionOpenDownloadSinkFlusher(t *testing.T) {
	w := httptest.NewRecorder()
	s := NewHTTPSession(w, httptest.NewRequest("GET", "/", nil))

	sink, flush, err := s.OpenDownloadSink()
	if err != nil {
		t.Fatalf("err = %v, want nil", err)
	}
	if sink != w {
		t.Error("sink is not the underlying ResponseWriter")
	}
	if err := flush(); err != nil {
		t.Errorf("flush() = %v, want nil", err)
	}
	if !w.Flushed {
		t.Error("flush() did not reach the underlying http.Flusher")
	}
}

// nonFlushingWriter implements only http.ResponseWriter, not http.Flusher, so
// OpenDownloadSink's no-op flush branch is reachable in a test.
type nonFlushingWriter struct {
	http.ResponseWriter
}

// TestHTTPSessionOpenDownloadSinkNoFlusher checks flush is a no-op (no panic,
// no error) when the writer does not implement http.Flusher.
func TestHTTPSessionOpenDownloadSinkNoFlusher(t *testing.T) {
	w := &nonFlushingWriter{ResponseWriter: httptest.NewRecorder()}
	s := NewHTTPSession(w, httptest.NewRequest("GET", "/", nil))

	_, flush, err := s.OpenDownloadSink()
	if err != nil {
		t.Fatalf("err = %v, want nil", err)
	}
	if err := flush(); err != nil {
		t.Errorf("flush() = %v, want nil", err)
	}
}
