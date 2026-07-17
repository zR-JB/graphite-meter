package transport

import (
	"bytes"
	"io"
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

func TestHTTPSessionOpenDownloadSink(t *testing.T) {
	w := httptest.NewRecorder()
	s := NewHTTPSession(w, httptest.NewRequest("GET", "/", nil))

	sink, err := s.OpenDownloadSink()
	if err != nil {
		t.Fatalf("err = %v, want nil", err)
	}
	if sink != w {
		t.Error("sink is not the underlying ResponseWriter")
	}
}
