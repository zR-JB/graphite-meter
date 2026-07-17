package endpoint

import (
	"bytes"
	"context"
	"crypto/rand"
	"io"
	"net/http"
	"net/http/httptest"
	"net/url"
	"strconv"
	"testing"
	"time"

	"github.com/zR-JB/graphite-meter/go/internal/transport"
)

// testBlockSize is a realistic download-block size for the wrap-around tests
// (matches the server's 256 KiB block).
const testBlockSize = 256 * 1024

// randomBlock returns n incompressible random bytes, like the server's shared
// download block.
func randomBlock(n int) []byte {
	b := make([]byte, n)
	if _, err := rand.Read(b); err != nil {
		panic(err)
	}
	return b
}

// newDownloadServer mounts /download (over the same httpAdapter the real mux
// uses) backed by a small block so block-wrap is easy to exercise.
func newDownloadServer(blockSize int) (*httptest.Server, []byte) {
	block := randomBlock(blockSize)
	mux := http.NewServeMux()
	mux.Handle("/download", httpAdapter(NewDownload(block, nil)))
	return httptest.NewServer(mux), block
}

func TestDownloadExactByteCount(t *testing.T) {
	srv, _ := newDownloadServer(testBlockSize)
	defer srv.Close()

	const want = 1 << 20 // 1 MiB
	res, err := http.Get(srv.URL + "/download?bytes=" + strconv.Itoa(want))
	if err != nil {
		t.Fatalf("get: %v", err)
	}
	defer res.Body.Close()

	if got := res.Header.Get("Content-Length"); got != strconv.Itoa(want) {
		t.Errorf("Content-Length = %q, want %d", got, want)
	}
	if got := res.Header.Get("Content-Type"); got != "application/octet-stream" {
		t.Errorf("Content-Type = %q", got)
	}
	if got := res.Header.Get("Cache-Control"); got != "no-store" {
		t.Errorf("Cache-Control = %q", got)
	}
	n, err := io.Copy(io.Discard, res.Body)
	if err != nil {
		t.Fatalf("read body: %v", err)
	}
	if n != want {
		t.Errorf("streamed %d bytes, want %d", n, want)
	}
}

func TestDownloadFirstByte(t *testing.T) {
	srv, _ := newDownloadServer(testBlockSize)
	defer srv.Close()
	client := srv.Client()
	client.Timeout = 2 * time.Second
	res, err := client.Get(srv.URL + "/download?bytes=" + strconv.FormatInt(maxBytes, 10))
	if err != nil {
		t.Fatalf("get: %v", err)
	}
	defer res.Body.Close()
	one := make([]byte, 1)
	if _, err := io.ReadFull(res.Body, one); err != nil {
		t.Fatalf("first byte: %v", err)
	}
}

func TestDownloadDeterministicAndIncompressible(t *testing.T) {
	srv, block := newDownloadServer(testBlockSize)
	defer srv.Close()

	const want = 300 << 10 // 300 KiB > block, so it wraps
	get := func() []byte {
		res, err := http.Get(srv.URL + "/download?bytes=" + strconv.Itoa(want))
		if err != nil {
			t.Fatalf("get: %v", err)
		}
		defer res.Body.Close()
		b, err := io.ReadAll(res.Body)
		if err != nil {
			t.Fatalf("read: %v", err)
		}
		return b
	}

	a, b := get(), get()
	if !bytes.Equal(a, b) {
		t.Fatal("two downloads differ — stream is not deterministic")
	}
	// Body must be the block, wrapping at the block boundary.
	if !bytes.Equal(a[:len(block)], block) {
		t.Error("first block-length of body != block")
	}
	if !bytes.Equal(a[len(block):], block[:want-len(block)]) {
		t.Error("post-wrap bytes do not continue from block start")
	}
}

func TestDownloadDefaultsAndClamps(t *testing.T) {
	if got := parseBytes(""); got != defaultBytes {
		t.Errorf("empty → %d, want default %d", got, defaultBytes)
	}
	if got := parseBytes("not-a-number"); got != defaultBytes {
		t.Errorf("invalid → %d, want default %d", got, defaultBytes)
	}
	if got := parseBytes("-5"); got != defaultBytes {
		t.Errorf("negative → %d, want default %d", got, defaultBytes)
	}
	if got := parseBytes(strconv.FormatInt(maxBytes+1, 10)); got != maxBytes {
		t.Errorf("over-max → %d, want clamp %d", got, maxBytes)
	}
	if got := parseBytes("0"); got != 0 {
		t.Errorf("zero → %d, want 0", got)
	}
}

func BenchmarkDownloadBlockSize(b *testing.B) {
	const size = 64 << 20
	for _, blockSize := range []int{64 << 10, 256 << 10, 1 << 20} {
		b.Run(strconv.Itoa(blockSize), func(b *testing.B) {
			download := NewDownload(randomBlock(blockSize), nil)
			session := &fakeSession{
				ctx:   context.Background(),
				query: "bytes=" + strconv.Itoa(size),
				sink:  io.Discard,
			}
			b.SetBytes(size)
			b.ReportAllocs()
			for i := 0; i < b.N; i++ {
				if err := download.Handle(session); err != nil {
					b.Fatal(err)
				}
			}
		})
	}
}

// TestDownloadContextCancel checks the stream stops promptly when the request
// context is cancelled mid-flight, without trying to fulfil the full length.
func TestDownloadContextCancel(t *testing.T) {
	block := randomBlock(4096)
	dl := NewDownload(block, nil)

	ctx, cancel := context.WithCancel(context.Background())
	// A sink that cancels the context after the first write, then keeps
	// counting: the loop must observe Done() and return rather than stream all
	// 10 MiB.
	sink := &cancelOnWrite{cancel: cancel}
	s := &fakeSession{ctx: ctx, query: "bytes=" + strconv.Itoa(10<<20), sink: sink}

	if err := dl.Handle(s); err != nil {
		t.Fatalf("handle: %v", err)
	}
	if sink.n >= int64(10<<20) {
		t.Errorf("wrote %d bytes — cancellation did not stop the stream", sink.n)
	}
}

/* ---- test doubles for the context-cancel path (no HTTP plumbing) ---- */

type cancelOnWrite struct {
	cancel context.CancelFunc
	once   bool
	n      int64
}

func (c *cancelOnWrite) Write(p []byte) (int, error) {
	if !c.once {
		c.once = true
		c.cancel()
	}
	c.n += int64(len(p))
	return len(p), nil
}

// fakeSession is a minimal transport.Session exposing only what Download.Handle
// uses: Context, Query, and OpenDownloadSink (non-HTTP, so headers are skipped).
type fakeSession struct {
	ctx   context.Context
	query string
	sink  io.Writer
}

func (f *fakeSession) Context() context.Context                         { return f.ctx }
func (f *fakeSession) Query() (v url.Values)                            { v, _ = url.ParseQuery(f.query); return }
func (f *fakeSession) Proto() transport.Proto                           { return transport.ProtoH1 }
func (f *fakeSession) HTTP() (http.ResponseWriter, *http.Request, bool) { return nil, nil, false }
func (f *fakeSession) OpenDownloadSink() (io.Writer, error) {
	return f.sink, nil
}
func (f *fakeSession) OpenUploadSource() (io.Reader, error) { return nil, transport.ErrUnsupported }
func (f *fakeSession) Bus() (transport.MessageBus, bool)    { return nil, false }
