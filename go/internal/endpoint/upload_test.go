package endpoint

import (
	"bytes"
	"context"
	"encoding/json/v2"
	"errors"
	"io"
	"net/http"
	"net/http/httptest"
	"strconv"
	"strings"
	"testing"
	"time"
)

func TestUploadCountsAndEchoes(t *testing.T) {
	mux := http.NewServeMux()
	store := NewUploadStore()
	id := store.Mint()
	mux.Handle("/upload", httpAdapter(NewUpload(nil, store)))
	srv := httptest.NewServer(mux)
	defer srv.Close()

	const n = 3*1024*1024 + 123 // straddles the drain buffer + a partial tail
	res, err := http.Post(srv.URL+"/upload?id="+id, "application/octet-stream", bytes.NewReader(make([]byte, n)))
	if err != nil {
		t.Fatalf("post: %v", err)
	}
	defer res.Body.Close()

	if got := res.Header.Get("Content-Type"); got != "application/json" {
		t.Errorf("Content-Type = %q", got)
	}
	if got := res.Header.Get("Cache-Control"); got != "no-store" {
		t.Errorf("Cache-Control = %q", got)
	}
	var echo struct {
		Bytes int64 `json:"bytes"`
	}
	if err := json.UnmarshalRead(res.Body, &echo); err != nil {
		t.Fatalf("decode echo: %v", err)
	}
	if echo.Bytes != n {
		t.Errorf("echoed %d bytes, want %d", echo.Bytes, n)
	}
}

func TestUploadAggregatesByID(t *testing.T) {
	store := NewUploadStore()
	id := store.Mint()

	mux := http.NewServeMux()
	mux.Handle("/upload", httpAdapter(NewUpload(nil, store)))
	srv := httptest.NewServer(mux)
	defer srv.Close()

	const n = 2*1024*1024 + 7
	res, err := http.Post(srv.URL+"/upload?id="+id, "application/octet-stream", bytes.NewReader(make([]byte, n)))
	if err != nil {
		t.Fatalf("post: %v", err)
	}
	res.Body.Close()

	agg, ok := store.get(id)
	if !ok {
		t.Fatal("aggregate missing after an id'd upload")
	}
	if got := agg.bytes.Load(); got != n {
		t.Errorf("aggregate bytes = %d, want %d", got, n)
	}
	if got := agg.elapsedNanos(monoNanos()); got < 0 {
		t.Errorf("elapsedNanos = %d, want >= 0", got)
	}
	if got := agg.posts.Load(); got != 0 {
		t.Errorf("posts = %d after the lane finished, want 0", got)
	}
}

func TestUploadForgedIDDoesNotAggregate(t *testing.T) {
	store := NewUploadStore()
	mux := http.NewServeMux()
	mux.Handle("/upload", httpAdapter(NewUpload(nil, store)))
	srv := httptest.NewServer(mux)
	defer srv.Close()

	res, err := http.Post(srv.URL+"/upload?id=forged", "application/octet-stream", bytes.NewReader(make([]byte, 4096)))
	if err != nil {
		t.Fatalf("post: %v", err)
	}
	res.Body.Close()

	if _, ok := store.get("forged"); ok {
		t.Error("a forged id created an aggregate")
	}
	if store.live.Load() != 0 {
		t.Errorf("live = %d after a forged-id upload, want 0", store.live.Load())
	}
}

func TestUploadAbortKeepsPartialAggregateAndDecrementsPosts(t *testing.T) {
	store := NewUploadStore()
	id := store.Mint()
	src := &errReader{remaining: 4096}
	if n, err := NewUpload(nil, store).HandleUpload(t.Context(), id, "", src); err == nil || n != 4096 {
		t.Fatalf("expected partial count and read error, got: %v", err)
	}

	agg, ok := store.get(id)
	if !ok {
		t.Fatal("aggregate missing after an aborted id'd upload")
	}
	if got := agg.bytes.Load(); got != 4096 {
		t.Errorf("aggregate bytes = %d, want 4096 (bytes drained before the abort)", got)
	}
	if got := agg.posts.Load(); got != 0 {
		t.Errorf("posts = %d after an aborted lane, want 0 (defer must run on the error path)", got)
	}
}

func TestUploadOverCapIDIsRejected(t *testing.T) {
	store := NewUploadStore()
	for i := range maxLiveUploads {
		id := store.Mint()
		if _, ok := store.getOrCreate(id); !ok {
			t.Fatalf("filler create %d below the cap was refused", i)
		}
	}
	id := store.Mint()

	mux := http.NewServeMux()
	mux.Handle("/upload", httpAdapter(NewUpload(nil, store)))
	srv := httptest.NewServer(mux)
	defer srv.Close()

	res, err := http.Post(srv.URL+"/upload?id="+id, "application/octet-stream", bytes.NewReader(make([]byte, 4096)))
	if err != nil {
		t.Fatalf("post: %v", err)
	}
	defer res.Body.Close()

	if res.StatusCode != http.StatusServiceUnavailable {
		t.Fatalf("status = %d, want 503", res.StatusCode)
	}
	if _, ok := store.get(id); ok {
		t.Error("an over-cap id unexpectedly got an aggregate")
	}
}

func TestUploadEmptyBodyIDCreatesZeroByteAggregate(t *testing.T) {
	store := NewUploadStore()
	id := store.Mint()

	mux := http.NewServeMux()
	mux.Handle("/upload", httpAdapter(NewUpload(nil, store)))
	srv := httptest.NewServer(mux)
	defer srv.Close()

	res, err := http.Post(srv.URL+"/upload?id="+id, "application/octet-stream", bytes.NewReader(nil))
	if err != nil {
		t.Fatalf("post: %v", err)
	}
	defer res.Body.Close()

	agg, ok := store.get(id)
	if !ok {
		t.Fatal("aggregate missing after an empty-body id'd upload")
	}
	if got := agg.bytes.Load(); got != 0 {
		t.Errorf("aggregate bytes = %d, want 0 for an empty body", got)
	}
	if got := agg.posts.Load(); got != 0 {
		t.Errorf("posts = %d after the empty-body lane finished, want 0", got)
	}
}

// A stream carries no status line, so a refused WebTransport upload lane can only be reported through Handle's return.
func TestUploadStreamRefusalIsReturnedAsAnError(t *testing.T) {
	store := NewUploadStore()
	for i := range maxLiveUploads {
		if _, ok := store.getOrCreate(store.Mint()); !ok {
			t.Fatalf("filler create %d below the cap was refused", i)
		}
	}
	id := store.Mint()
	// The stream boundary returns its refusal before consuming the reader.
	src := bytes.NewReader(make([]byte, 4096))

	_, err := NewUpload(nil, store).HandleUpload(t.Context(), id, "", src)

	if err == nil {
		t.Fatal("a refused stream lane returned nil: the peer is never told and its bytes are never counted")
	}
	if want := uploadAccessMessage(uploadAccessGlobalFull); !strings.Contains(err.Error(), want) {
		t.Fatalf("refusal = %q, want it to carry %q", err, want)
	}
	if _, ok := store.get(id); ok {
		t.Error("the refused id got an aggregate anyway")
	}
}

// deadlineRecorder is a ResponseWriter that records what http.NewResponseController(w).SetReadDeadline was handed.
type deadlineRecorder struct {
	http.ResponseWriter
	read time.Time
	set  bool
}

func (d *deadlineRecorder) SetReadDeadline(t time.Time) error {
	d.read, d.set = t, true
	return nil
}

// A POST body that stops arriving mid-upload holds a goroutine and a 256 KiB drain buffer.
func TestUploadBoundsAStuckBodyRead(t *testing.T) {
	rec := &deadlineRecorder{ResponseWriter: httptest.NewRecorder()}
	store := NewUploadStore()
	req := httptest.NewRequest(http.MethodPost, "/upload?id="+store.Mint(), bytes.NewReader(make([]byte, 4096)))
	before := time.Now()

	if err := NewUpload(nil, store).HandleHTTP(rec, req); err != nil {
		t.Fatalf("handle: %v", err)
	}

	if !rec.set {
		t.Fatal("no read deadline was set: a stuck POST body would pin its goroutine and drain buffer indefinitely")
	}
	if got := rec.read.Sub(before); got < uploadReadTimeout || got > uploadReadTimeout+time.Minute {
		t.Fatalf("read deadline is %v out, want about %v", got, uploadReadTimeout)
	}
}

func TestUploadRespectsRequestDeadline(t *testing.T) {
	for _, remaining := range []time.Duration{-time.Second, time.Second, time.Hour} {
		t.Run(remaining.String(), func(t *testing.T) {
			requestDeadline := time.Now().Add(remaining)
			ctx, cancel := context.WithDeadline(t.Context(), requestDeadline)
			defer cancel()
			rec := &deadlineRecorder{ResponseWriter: httptest.NewRecorder()}
			store := NewUploadStore()
			req := httptest.NewRequestWithContext(ctx, http.MethodPost, "/upload?id="+store.Mint(), nil)
			before := time.Now()
			if err := NewUpload(nil, store).HandleHTTP(rec, req); err != nil {
				t.Fatal(err)
			}
			if !rec.set || rec.read.After(requestDeadline) {
				t.Fatalf("read deadline %v exceeds request deadline %v", rec.read, requestDeadline)
			}
			if remaining < uploadReadTimeout {
				if !rec.read.Equal(requestDeadline) {
					t.Fatalf("read deadline = %v, want %v", rec.read, requestDeadline)
				}
			} else if rec.read.Before(before.Add(uploadReadTimeout)) || rec.read.After(time.Now().Add(uploadReadTimeout)) {
				t.Fatalf("read deadline %v does not retain the upload timeout", rec.read)
			}
		})
	}
}

func TestDiscardSinkHasNoReaderFrom(t *testing.T) {
	if _, ok := io.Writer(discardSink{}).(io.ReaderFrom); ok {
		t.Error("discardSink must not implement io.ReaderFrom")
	}
}

func BenchmarkUploadBufferSize(b *testing.B) {
	const size = 64 << 20
	source := bytes.Repeat([]byte{1}, size)
	for _, bufferSize := range []int{32 << 10, 256 << 10, 1 << 20} {
		b.Run(strconv.Itoa(bufferSize), func(b *testing.B) {
			buffer := make([]byte, bufferSize)
			reader := bytes.NewReader(source)
			b.SetBytes(size)
			b.ReportAllocs()
			for b.Loop() {
				reader.Reset(source)
				if _, err := io.CopyBuffer(discardSink{agg: new(uploadAgg)}, io.LimitReader(reader, size), buffer); err != nil {
					b.Fatal(err)
				}
			}
		})
	}
}

/* ---- test doubles ---- */

// errReader yields `remaining` zero bytes then a non-EOF error, simulating a connection dropped mid-upload.
type errReader struct{ remaining int }

func (r *errReader) Read(p []byte) (int, error) {
	if r.remaining <= 0 {
		return 0, errors.New("simulated connection reset")
	}
	n := min(len(p), r.remaining)
	r.remaining -= n
	return n, nil
}

func TestUploadStreamOwnerCannotReadAnotherClientsLane(t *testing.T) {
	store := NewUploadStore()
	id := store.Mint()
	upload := NewUpload(nil, store)
	if n, err := upload.HandleUpload(t.Context(), id, "owner", strings.NewReader("first")); err != nil || n != 5 {
		t.Fatalf("initial upload = %d, %v", n, err)
	}
	src := strings.NewReader("must not be read")
	n, err := upload.HandleUpload(t.Context(), id, "other-owner", src)
	refusal, ok := errors.AsType[*uploadRefusalError](err)
	if !ok || refusal.access != uploadAccessOwnerMismatch || n != 0 || src.Len() != len("must not be read") {
		t.Fatalf("refused upload = %d, %v; unread bytes = %d", n, err, src.Len())
	}
	agg, _ := store.get(id)
	if agg.bytes.Load() != 5 || agg.posts.Load() != 0 {
		t.Fatalf("refusal changed aggregate: bytes=%d posts=%d", agg.bytes.Load(), agg.posts.Load())
	}
}

func TestUploadHTTPAbortDoesNotPublishCompleteBytes(t *testing.T) {
	store := NewUploadStore()
	id := store.Mint()
	req := httptest.NewRequest(http.MethodPost, "/upload?id="+id, &errReader{remaining: 4096})
	rec := httptest.NewRecorder()
	if err := NewUpload(nil, store).HandleHTTP(rec, req); err != nil {
		t.Fatal(err)
	}
	if rec.Body.Len() != 0 {
		t.Fatalf("aborted upload published response %q", rec.Body.String())
	}
	agg, ok := store.get(id)
	if !ok || agg.bytes.Load() != 4096 || agg.posts.Load() != 0 {
		t.Fatal("aborted HTTP upload lost its partial receiver count or retained its lane")
	}
}

func TestUploadHTTPRequiresOwnerBoundIDBeforeReading(t *testing.T) {
	for _, candidate := range []string{"", "forged", "another-owner"} {
		t.Run(candidate, func(t *testing.T) {
			store := NewUploadStore()
			id := candidate
			if candidate == "another-owner" {
				id = store.Mint()
				if _, access := store.getOrCreateFor(id, "different-owner"); access != uploadAccessOK {
					t.Fatal(access)
				}
			}
			body := strings.NewReader("must not be drained")
			req := httptest.NewRequest(http.MethodPost, "/upload?id="+id, body)
			rec := httptest.NewRecorder()
			if err := NewUpload(nil, store).HandleHTTP(rec, req); err != nil {
				t.Fatal(err)
			}
			if rec.Code < 400 || body.Len() != len("must not be drained") {
				t.Fatalf("refusal = %d, unread = %d", rec.Code, body.Len())
			}
		})
	}
}
