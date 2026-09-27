package endpoint

import (
	"bytes"
	"context"
	"crypto/rand"
	"io"
	"net/http"
	"net/http/httptest"
	"strconv"
	"testing"
	"time"

	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

func randomBlock(n int) []byte {
	b := make([]byte, n)
	_, _ = rand.Read(b)
	return b
}

func TestDownloadStreamsTheWrappedBlock(t *testing.T) {
	block := randomBlock(256 * 1024)
	meter := NewMeter("test:download")
	srv := httptest.NewServer(NewDownload(block, meter).Handler(wire.IdleBound))
	defer srv.Close()
	head, err := http.Head(srv.URL + "/download?bytes=" + strconv.FormatInt(maxBytes, 10))
	if err != nil {
		t.Fatalf("head: %v", err)
	}
	head.Body.Close()
	// A HEAD that streamed the largest body would still be running and counting.
	if head.StatusCode != http.StatusOK || head.ContentLength != maxBytes || meter.bytes.Load() != 0 ||
		meter.conns.Load() != 0 {
		t.Fatalf("HEAD = %d with length %d; meter bytes=%d conns=%d", head.StatusCode, head.ContentLength,
			meter.bytes.Load(), meter.conns.Load())
	}
	const want = 300 << 10
	get := func() []byte {
		res, err := http.Get(srv.URL + "/download?bytes=" + strconv.Itoa(want))
		if err != nil {
			t.Fatalf("get: %v", err)
		}
		defer res.Body.Close()
		if res.Header.Get("Content-Length") != strconv.Itoa(want) || res.Header.Get("Cache-Control") != "no-store" ||
			res.Header.Get("Content-Type") != "application/octet-stream" {
			t.Fatalf("headers = %v", res.Header)
		}
		b, err := io.ReadAll(res.Body)
		if err != nil || len(b) != want {
			t.Fatalf("read %d bytes: %v", len(b), err)
		}
		return b
	}
	a, b := get(), get()
	if !bytes.Equal(a, b) || !bytes.Equal(a[:len(block)], block) ||
		!bytes.Equal(a[len(block):], block[:want-len(block)]) {
		t.Fatal("downloads are not the block repeated from its start")
	}
	client := srv.Client()
	client.Timeout = 2 * time.Second
	res, err := client.Get(srv.URL + "/download?bytes=" + strconv.FormatInt(maxBytes, 10))
	if err != nil {
		t.Fatalf("get: %v", err)
	}
	defer res.Body.Close()
	if _, err := io.ReadFull(res.Body, make([]byte, 1)); err != nil {
		t.Fatalf("first byte of the largest download: %v", err)
	}
}

type cancelOnWrite struct {
	cancel context.CancelFunc
	n      int64
}

func (c *cancelOnWrite) Write(p []byte) (int, error) {
	c.cancel()
	c.n += int64(len(p))
	return len(p), nil
}

func TestDownloadContextCancel(t *testing.T) {
	ctx, cancel := context.WithCancel(t.Context())
	sink := &cancelOnWrite{cancel: cancel}
	NewDownload(randomBlock(4096), nil).Stream(ctx, 10<<20, sink)
	if sink.n >= 10<<20 {
		t.Errorf("wrote %d bytes, cancellation did not stop the stream", sink.n)
	}
}

func BenchmarkDownloadBlockSize(b *testing.B) {
	const size = 64 << 20
	for _, blockSize := range []int{64 << 10, 256 << 10, 1 << 20} {
		b.Run(strconv.Itoa(blockSize), func(b *testing.B) {
			download := NewDownload(randomBlock(blockSize), nil)
			b.SetBytes(size)
			b.ReportAllocs()
			for b.Loop() {
				download.Stream(b.Context(), size, io.Discard)
			}
		})
	}
}
