package endpoint

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"io"
	"net/http"
	"net/http/httptest"
	"strconv"
	"testing"
)

func TestUploadCountsAndEchoes(t *testing.T) {
	mux := http.NewServeMux()
	mux.Handle("/upload", httpAdapter(NewUpload(nil, nil)))
	srv := httptest.NewServer(mux)
	defer srv.Close()

	const n = 3*1024*1024 + 123 // straddles the drain buffer + a partial tail
	res, err := http.Post(srv.URL+"/upload", "application/octet-stream", bytes.NewReader(make([]byte, n)))
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
	if err := json.NewDecoder(res.Body).Decode(&echo); err != nil {
		t.Fatalf("decode echo: %v", err)
	}
	if echo.Bytes != n {
		t.Errorf("echoed %d bytes, want %d", echo.Bytes, n)
	}
}

// TestUploadAggregatesByID checks that a POST carrying a server-minted ?id= adds
// its drained bytes to the shared per-id aggregate (the server-authoritative count
// the /upload/progress bus reports) and accrues active measurement time.
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

// TestUploadForgedIDDoesNotAggregate checks that an unauthenticated id creates no state.
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

// TestUploadStopsOnReadError checks a mid-stream read failure (the client
// aborting a streaming upload) returns cleanly without echoing.
func TestUploadStopsOnReadError(t *testing.T) {
	s := &uploadSession{
		fakeSession: &fakeSession{ctx: context.Background()},
		src:         &errReader{remaining: 4096},
	}
	if err := NewUpload(nil, nil).Handle(s); err != nil {
		t.Fatalf("handle should swallow the abort, got: %v", err)
	}
}

// TestUploadAbortKeepsPartialAggregateAndDecrementsPosts checks a mid-stream
// abort on an id'd upload: bytes drained before the error are kept (never
// rolled back), and posts is decremented via defer even on the error path —
// the lane-count invariant must hold whether Handle returns via EOF or error.
func TestUploadAbortKeepsPartialAggregateAndDecrementsPosts(t *testing.T) {
	store := NewUploadStore()
	id := store.Mint()
	s := &uploadSession{
		fakeSession: &fakeSession{ctx: context.Background(), query: "id=" + id},
		src:         &errReader{remaining: 4096},
	}
	if err := NewUpload(nil, store).Handle(s); err != nil {
		t.Fatalf("handle should swallow the abort, got: %v", err)
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

// TestUploadOverCapIDIsRejected ensures a saturated authoritative aggregate
// store never degrades into a plausible client-counted upload result.
func TestUploadOverCapIDIsRejected(t *testing.T) {
	store := NewUploadStore()
	for i := 0; i < maxLiveUploads; i++ {
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

// TestUploadEmptyBodyIDCreatesZeroByteAggregate checks a zero-byte POST for
// an id'd upload still creates an aggregate — the POST itself is the first
// touch — reporting zero bytes, since a zero-length read never reaches
// discardSink.Write and so never calls recordChunk.
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

// TestDiscardSinkHasNoReaderFrom guards the buffer-control invariant: if
// discardSink implemented io.ReaderFrom, io.CopyBuffer would bypass our large
// pooled buffer for io.Discard's small internal one.
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
			for i := 0; i < b.N; i++ {
				reader.Reset(source)
				if _, err := io.CopyBuffer(discardSink{}, io.LimitReader(reader, size), buffer); err != nil {
					b.Fatal(err)
				}
			}
		})
	}
}

/* ---- test doubles ---- */

// uploadSession reuses the download test's fakeSession (same package) and only
// overrides the upload source.
type uploadSession struct {
	*fakeSession
	src io.Reader
}

func (u *uploadSession) OpenUploadSource() (io.Reader, error) { return u.src, nil }

// errReader yields `remaining` zero bytes then a non-EOF error, simulating a
// connection dropped mid-upload.
type errReader struct{ remaining int }

func (r *errReader) Read(p []byte) (int, error) {
	if r.remaining <= 0 {
		return 0, errors.New("simulated connection reset")
	}
	n := len(p)
	if n > r.remaining {
		n = r.remaining
	}
	r.remaining -= n
	return n, nil
}
