package endpoint

import (
	"io"
	"strconv"
	"sync"

	"github.com/zR-JB/graphite-meter/server/internal/transport"
)

// Upload sinks the client's streamed bytes for the upload measurement: it drains
// the request body to io.Discard through a pooled scratch buffer, counting but
// never accumulating (docs/ARCHITECTURE.md §7). The client measures bytes sent;
// the server only needs to consume them as fast as the link allows and may echo
// the total.
type Upload struct{}

// NewUpload builds the upload endpoint.
func NewUpload() *Upload { return &Upload{} }

func (u *Upload) ID() string                 { return "upload" }
func (u *Upload) Capabilities() Capabilities { return Capabilities{HTTP: true} }

// uploadBufSize is the drain buffer per in-flight upload. Larger than
// io.Discard's internal 8 KiB so a saturated link costs far fewer read syscalls.
const uploadBufSize = 256 * 1024

// scratchPool reuses drain buffers across uploads — zero per-request allocation.
// Stores *[]byte so Get/Put don't box the slice header.
var scratchPool = sync.Pool{
	New: func() any {
		b := make([]byte, uploadBufSize)
		return &b
	},
}

// discardSink counts via the io.Copy return value while throwing the bytes away.
// It deliberately does NOT implement io.ReaderFrom, so io.CopyBuffer uses our
// large pooled buffer instead of io.Discard's small internal one.
type discardSink struct{}

func (discardSink) Write(p []byte) (int, error) { return len(p), nil }

// Handle drains the upload source, counting bytes. A clean EOF echoes the count
// as JSON; a mid-stream cancel (client aborted the measurement) stops quietly.
func (u *Upload) Handle(s transport.Session) error {
	src, err := s.OpenUploadSource()
	if err != nil {
		return err
	}

	bufp := scratchPool.Get().(*[]byte)
	defer scratchPool.Put(bufp)

	// CopyBuffer reads until EOF or a read error; the http server cancels the
	// body read when the request context is cancelled, so this returns promptly
	// on client disconnect.
	n, copyErr := io.CopyBuffer(discardSink{}, src, *bufp)
	if copyErr != nil {
		// Client aborted the stream (the common case for a streaming upload
		// measurement) — the connection is gone, so there's nothing to reply to.
		return nil
	}

	if w, _, ok := s.HTTP(); ok {
		h := w.Header()
		h.Set("Content-Type", "application/json")
		h.Set("Cache-Control", "no-store")
		_, _ = io.WriteString(w, `{"bytes":`+strconv.FormatInt(n, 10)+`}`)
	}
	return nil
}
