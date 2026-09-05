package endpoint

import (
	"context"
	"errors"
	"io"
	"net/http"
	"net/netip"
	"strconv"
	"sync"
	"time"
)

// Upload sinks the client's streamed bytes for the upload measurement.
type Upload struct {
	meter   *Meter       // optional verbose per-second logger; nil unless -verbose
	store   *UploadStore // required owner-bound receiver aggregate
	trusted []netip.Prefix
}

// uploadReadTimeout bounds a single stuck POST's body read so a half-open lane cannot pin a goroutine indefinitely.
const uploadReadTimeout = 120 * time.Second

// NewUpload requires an aggregate store. meter may be nil (no verbose logging).
func NewUpload(meter *Meter, store *UploadStore, trusted ...[]netip.Prefix) *Upload {
	return &Upload{meter: meter, store: store, trusted: optionalPrefixes(trusted)}
}

const uploadBufSize = 256 * 1024

var scratchPool = sync.Pool{
	New: func() any {
		return new(make([]byte, uploadBufSize))
	},
}

type discardSink struct {
	meter *Meter
	agg   *uploadAgg
}

func (s discardSink) Write(p []byte) (int, error) {
	s.meter.Add(len(p))
	s.agg.recordChunk(monoNanos(), len(p))
	return len(p), nil
}

// HandleHTTP owns HTTP deadlines, refusal status, and the final byte response.
func (u *Upload) HandleHTTP(w http.ResponseWriter, r *http.Request) error {
	deadline := time.Now().Add(uploadReadTimeout)
	if requestDeadline, ok := r.Context().Deadline(); ok && requestDeadline.Before(deadline) {
		deadline = requestDeadline
	}
	_ = http.NewResponseController(w).SetReadDeadline(deadline)
	n, err := u.HandleUpload(r.Context(), r.URL.Query().Get("id"), ClientKey(r, u.trusted), r.Body)
	if err != nil {
		if refusal, ok := errors.AsType[*uploadRefusalError](err); ok {
			writeUploadAccessError(w, refusal.access)
		}
		return nil // A failed body read means the client aborted its lane.
	}
	h := w.Header()
	h.Set("Content-Type", "application/json")
	h.Set("Cache-Control", "no-store")
	_, _ = io.WriteString(w, `{"bytes":`+strconv.FormatInt(n, 10)+`}`)
	return nil
}

// HandleUpload joins the owner's aggregate before reading, and records receiver-side chunks and timing.
func (u *Upload) HandleUpload(_ context.Context, id, owner string, src io.Reader) (int64, error) {
	agg, access := u.store.getOrCreateFor(id, owner)
	if access != uploadAccessOK {
		return 0, &uploadRefusalError{access: access}
	}
	agg.changePosts(1)
	defer agg.changePosts(-1)
	bufp := scratchPool.Get().(*[]byte)
	defer scratchPool.Put(bufp)
	u.meter.Open()
	defer u.meter.Close()
	return io.CopyBuffer(discardSink{meter: u.meter, agg: agg}, src, *bufp)
}
