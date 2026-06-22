package endpoint

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"io"
	"net/http"
	"net/http/httptest"
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
// the /ws/upload bus reports) and stamps the first-byte time.
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
	if agg.firstByteMono.Load() == 0 {
		t.Error("firstByteMono not stamped")
	}
	if got := agg.posts.Load(); got != 0 {
		t.Errorf("posts = %d after the lane finished, want 0", got)
	}
}

// TestUploadUnissuedIDDoesNotAggregate checks the abuse path: a forged (unminted)
// id drains-and-counts normally but creates no server-side aggregate.
func TestUploadUnissuedIDDoesNotAggregate(t *testing.T) {
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

// TestDiscardSinkHasNoReaderFrom guards the buffer-control invariant: if
// discardSink implemented io.ReaderFrom, io.CopyBuffer would bypass our large
// pooled buffer for io.Discard's small internal one.
func TestDiscardSinkHasNoReaderFrom(t *testing.T) {
	if _, ok := io.Writer(discardSink{}).(io.ReaderFrom); ok {
		t.Error("discardSink must not implement io.ReaderFrom")
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
