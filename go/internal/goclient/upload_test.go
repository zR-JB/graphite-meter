package goclient

import (
	"bytes"
	"context"
	"encoding/json/jsontext"
	jsonv2 "encoding/json/v2"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

// A request body cycles one random block: exactly its limit, then EOF; a cancelled request stops reading.
func TestCyclingBody(t *testing.T) {
	t.Parallel()
	for _, c := range []struct {
		limit int64
		want  []byte
	}{
		{0, []byte{1, 2, 3, 1, 2, 3, 1, 2, 3, 1, 2, 3}},
		{7, []byte{1, 2, 3, 1, 2, 3, 1}},
	} {
		b := &cyclingBody{ctx: t.Context(), block: []byte{1, 2, 3}, limit: c.limit}
		var got []byte
		buf := make([]byte, 4)
		for len(got) < 12 {
			n, err := b.Read(buf)
			got = append(got, buf[:n]...)
			if errors.Is(err, io.EOF) {
				break
			}
		}
		if !bytes.Equal(got, c.want) {
			t.Errorf("limit %d emitted %v, want %v", c.limit, got, c.want)
		}
	}
	ctx, cancel := context.WithCancel(t.Context())
	cancel()
	if _, err := (&cyclingBody{ctx: ctx, block: []byte{1}}).Read(make([]byte, 4)); err == nil {
		t.Fatal("a cancelled request kept reading")
	}
}

func TestMintUploadID(t *testing.T) {
	t.Parallel()
	for body, want := range map[string]string{
		`{"uploadId":"abc-123"}`: "abc-123",
		`{}`:                     "",
		`not json`:               "",
		"":                       "", // HTTP 500 below.
	} {
		srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
			if body == "" {
				w.WriteHeader(http.StatusInternalServerError)
				return
			}
			_, _ = io.WriteString(w, body)
		}))
		id, err := (&runner{cfg: Config{BaseURL: srv.URL}, http: srv.Client()}).mintUploadID(t.Context())
		srv.Close()
		if id != want || (err == nil) != (want != "") {
			t.Errorf("session response %q minted %q, %v; want %q", body, id, err, want)
		}
	}
}

func TestUploadLaneDrainsBytes(t *testing.T) {
	t.Parallel()
	var served atomic.Uint64
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		buf := make([]byte, 32*1024)
		for {
			n, err := r.Body.Read(buf)
			if n > 0 {
				served.Add(uint64(n))
			}
			if err != nil {
				return
			}
		}
	}))
	defer srv.Close()

	r := &runner{cfg: Config{BaseURL: srv.URL}, http: srv.Client()}
	block := make([]byte, 64*1024)

	ctx, cancel := context.WithCancel(t.Context())
	done := make(chan struct{})
	go func() {
		_ = r.uploadLane(ctx, "test-id", 0, block, func() {})
		close(done)
	}()

	const threshold = 3 * 64 * 1024 // several multiples of the block size
	deadline := time.After(2 * time.Second)
	for served.Load() < threshold {
		select {
		case <-deadline:
			cancel()
			t.Fatalf("server only observed %d bytes, want at least %d", served.Load(), threshold)
		case <-time.After(5 * time.Millisecond):
		}
	}

	cancel()
	select {
	case <-done:
	case <-time.After(2 * time.Second):
		t.Fatal("uploadLane did not return after context cancellation")
	}
}

func TestUploadLaneReturnsAdmissionRejection(t *testing.T) {
	t.Parallel()
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		w.WriteHeader(http.StatusServiceUnavailable)
	}))
	defer srv.Close()
	r := &runner{cfg: Config{BaseURL: srv.URL}, http: srv.Client()}
	if err := r.uploadLane(t.Context(), "test-id", 0, make([]byte, 1024), func() {}); err == nil {
		t.Fatal("HTTP 503 did not fail the upload lane")
	}
}

func newAbruptCloseUploadServer(requests *atomic.Int64) *httptest.Server {
	return httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		requests.Add(1)
		buf := make([]byte, 8*1024)
		_, _ = r.Body.Read(buf)
		panic(http.ErrAbortHandler)
	}))
}

func TestUploadLaneSurvivesAbruptConnectionDrop(t *testing.T) {
	t.Parallel()
	var requests atomic.Int64
	srv := newAbruptCloseUploadServer(&requests)
	defer srv.Close()

	r := &runner{cfg: Config{BaseURL: srv.URL}, http: srv.Client()}
	block := make([]byte, 64*1024)

	ctx, cancel := context.WithCancel(t.Context())
	done := make(chan struct{})
	go func() {
		_ = r.uploadLane(ctx, "test-id", 0, block, func() {})
		close(done)
	}()

	defer cancel()
	deadline := time.After(2 * time.Second)
	for requests.Load() < 2 {
		select {
		case <-done:
			t.Fatal("upload lane stopped instead of retrying the dropped connection")
		case <-deadline:
			t.Fatal("upload lane did not reopen after the dropped connection")
		case <-time.After(5 * time.Millisecond):
		}
	}
	cancel()
	select {
	case <-done:
	case <-time.After(2 * time.Second):
		t.Fatal("uploadLane did not return after repeated abrupt connection drops plus cancellation")
	}
}

func mountFakeProgress(mux *http.ServeMux, served *atomic.Uint64, started time.Time) {
	mux.HandleFunc("/upload/checkpoint", func(w http.ResponseWriter, r *http.Request) {
		_ = jsonv2.MarshalWrite(w, struct {
			Bytes uint64 `json:"bytes"`
			Nanos uint64 `json:"nanos"`
		}{served.Load(), uint64(time.Since(started))})
	})
	finished := make(chan struct{})
	var once sync.Once
	mux.HandleFunc("/upload/progress", func(w http.ResponseWriter, r *http.Request) {
		if r.Method == http.MethodDelete {
			once.Do(func() { close(finished) })
			w.WriteHeader(http.StatusNoContent)
			return
		}
		w.Header().Set("Content-Type", "application/x-ndjson")
		flusher := w.(http.Flusher)
		enc := jsontext.NewEncoder(w)
		_ = jsonv2.MarshalEncode(enc, wire.UploadProgress{Type: "ready"})
		flusher.Flush()
		ticker := time.Tick(20 * time.Millisecond)
		for {
			select {
			case <-r.Context().Done():
				return
			case <-finished:
				_ = jsonv2.MarshalEncode(enc, wire.UploadProgress{Type: "complete", Bytes: served.Load(), Nanos: uint64(time.Since(started))})
				flusher.Flush()
				return
			case <-ticker:
				_ = jsonv2.MarshalEncode(enc, wire.UploadProgress{Type: "progress", Bytes: served.Load(), Nanos: uint64(time.Since(started))})
				flusher.Flush()
			}
		}
	})
}

func newFakeUploadServer(t *testing.T) *httptest.Server {
	t.Helper()
	var served atomic.Uint64
	started := time.Now()

	mux := http.NewServeMux()
	mux.HandleFunc("/upload/session", func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		_ = jsonv2.MarshalWrite(w, uploadSessionResponse{UploadID: "test-upload"})
	})
	mux.HandleFunc("/upload", func(w http.ResponseWriter, r *http.Request) {
		buf := make([]byte, 32*1024)
		for {
			n, err := r.Body.Read(buf)
			if n > 0 {
				served.Add(uint64(n))
			}
			if err != nil {
				return
			}
		}
	})
	mountFakeProgress(mux, &served, started)
	return httptest.NewServer(mux)
}

func TestMeasureUploadReportsServerAuthoritativeTotal(t *testing.T) {
	t.Parallel()
	srv := newFakeUploadServer(t)
	defer srv.Close()

	cfg := Config{
		BaseURL:         srv.URL,
		TransferStreams: TransferStreamPolicy{Forced: 1},
	}.normalized()
	r := &runner{cfg: cfg, streams: streamCounts{down: 1, up: 1}, http: srv.Client(), emit: func(Event) {}}
	attachTestLatencyTarget(r, srv.URL)

	ctx, cancel := context.WithTimeout(t.Context(), 5*time.Second)
	defer cancel()

	res, err := r.testTransferResult(ctx, "upload", 300*time.Millisecond)
	if err != nil {
		t.Fatalf("measureUpload: %v", err)
	}
	if res.TotalBytes == 0 {
		t.Error("reported TotalBytes = 0, want > 0")
	}
}

func newStalledUploadServer() *httptest.Server {
	started := time.Now()
	mux := http.NewServeMux()
	mux.HandleFunc("/upload/checkpoint", func(w http.ResponseWriter, _ *http.Request) {
		_, _ = fmt.Fprintf(w, `{"bytes":0,"nanos":%d}`, time.Since(started))
	})
	mux.HandleFunc("/upload/session", func(w http.ResponseWriter, _ *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		_ = jsonv2.MarshalWrite(w, uploadSessionResponse{UploadID: "stalled-upload"})
	})
	mux.HandleFunc("/upload", func(_ http.ResponseWriter, r *http.Request) {
		_, _ = io.Copy(io.Discard, r.Body)
	})
	mux.HandleFunc("/upload/progress", func(w http.ResponseWriter, r *http.Request) {
		if r.Method == http.MethodDelete {
			w.WriteHeader(http.StatusNoContent)
			return
		}
		w.Header().Set("Content-Type", "application/x-ndjson")
		flusher := w.(http.Flusher)
		enc := jsontext.NewEncoder(w)
		_ = jsonv2.MarshalEncode(enc, wire.UploadProgress{Type: "ready"})
		flusher.Flush()
		ticker := time.Tick(20 * time.Millisecond)
		for {
			select {
			case <-r.Context().Done():
				return
			case <-ticker:
				_ = jsonv2.MarshalEncode(enc, wire.UploadProgress{Type: "progress", Bytes: 0, Nanos: uint64(time.Since(started))})
				flusher.Flush()
			}
		}
	})
	return httptest.NewServer(mux)
}

func TestMeasureUploadCancelledEmptyWindowIsACleanStop(t *testing.T) {
	t.Parallel()
	srv := newStalledUploadServer()
	defer srv.Close()

	cfg := Config{BaseURL: srv.URL, TransferStreams: TransferStreamPolicy{Forced: 1}}.normalized()
	r := &runner{cfg: cfg, streams: streamCounts{down: 1, up: 1}, http: srv.Client(), emit: func(Event) {}}
	attachTestLatencyTarget(r, srv.URL)

	ctx, cancel := context.WithCancel(t.Context())
	defer cancel()
	time.AfterFunc(200*time.Millisecond, cancel)

	_, err := r.testTransferResult(ctx, "upload", 5*time.Second)
	if !errors.Is(err, context.Canceled) {
		t.Fatalf("cancelled stage returned %v, want context.Canceled", err)
	}
	if strings.Contains(err.Error(), "carried no bytes") {
		t.Errorf("err = %v, want the cancellation reported as a stop", err)
	}
}
