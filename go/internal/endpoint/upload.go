package endpoint

import (
	"context"
	"encoding/json/v2"
	"errors"
	"io"
	"net/http"
	"net/netip"
	"strconv"
	"sync"
	"time"
)

// Upload counts the client's streamed bytes into owner-bound receivers and
// serves the routes that mint, observe and finish them. The receiver, not the
// sender, is the authority on how many bytes arrived and when.
type Upload struct {
	meter   *Meter // optional verbose per-second logger; nil unless -verbose
	store   *UploadStore
	trusted []netip.Prefix
}

// uploadReadTimeout bounds a single stuck lane's body read so a half-open lane cannot pin a goroutine indefinitely.
const uploadReadTimeout = 120 * time.Second

// NewUpload requires a receiver store. meter may be nil (no verbose logging).
func NewUpload(meter *Meter, store *UploadStore, trusted []netip.Prefix) *Upload {
	return &Upload{meter: meter, store: store, trusted: trusted}
}

const uploadBufSize = 256 * 1024

var scratchPool = sync.Pool{
	New: func() any {
		return new(make([]byte, uploadBufSize))
	},
}

// discardSink records every chunk on the receiver. It deliberately has no
// ReadFrom, so io.CopyBuffer reads through the pooled scratch buffer.
type discardSink struct {
	meter *Meter
	agg   *uploadAgg
}

func (s discardSink) Write(p []byte) (int, error) {
	s.meter.Add(len(p))
	s.agg.recordChunk(monoNanos(), len(p))
	return len(p), nil
}

// ServeHTTP drains one POST /upload lane, owning its read deadline, refusal status and final byte response.
func (u *Upload) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	deadline := time.Now().Add(uploadReadTimeout)
	if requestDeadline, ok := r.Context().Deadline(); ok && requestDeadline.Before(deadline) {
		deadline = requestDeadline
	}
	_ = http.NewResponseController(w).SetReadDeadline(deadline)
	n, err := u.Receive(r.Context(), r.URL.Query().Get("id"), UploadOwner(r, u.trusted), r.Body)
	if err != nil {
		if refusal, ok := errors.AsType[*uploadRefusalError](err); ok {
			writeUploadAccessError(w, refusal.access)
		}
		return // A failed body read means the client aborted its lane.
	}
	h := w.Header()
	h.Set("Content-Type", "application/json")
	h.Set("Cache-Control", "no-store")
	_, _ = io.WriteString(w, `{"bytes":`+strconv.FormatInt(n, 10)+`}`)
}

// Receive joins the owner's receiver before reading and records receiver-side chunks and timing.
func (u *Upload) Receive(_ context.Context, id, owner string, src io.Reader) (int64, error) {
	agg, access := u.store.joinPostFor(id, owner)
	if access != uploadAccessOK {
		return 0, &uploadRefusalError{access: access}
	}
	defer agg.endPost()
	bufp := scratchPool.Get().(*[]byte)
	defer scratchPool.Put(bufp)
	u.meter.Open()
	defer u.meter.Close()
	return io.CopyBuffer(discardSink{meter: u.meter, agg: agg}, src, *bufp)
}

// ServeSession mints the short-lived upload correlation token without storing per-token state.
func (u *Upload) ServeSession(w http.ResponseWriter, _ *http.Request) {
	w.Header().Set("Content-Type", "application/json")
	w.Header().Set("Cache-Control", "no-store")
	_ = json.MarshalWrite(w, struct {
		UploadID string `json:"uploadId"`
	}{u.store.Mint()})
}

// ServeCheckpoint reports an existing receiver's counter without keeping it alive.
func (u *Upload) ServeCheckpoint(w http.ResponseWriter, r *http.Request) {
	agg, found := u.store.get(r.URL.Query().Get("id"))
	if !found {
		writeUploadAccessError(w, uploadAccessInvalid)
		return
	}
	if agg.owner != "" && agg.owner != UploadOwner(r, u.trusted) {
		writeUploadAccessError(w, uploadAccessOwnerMismatch)
		return
	}
	w.Header().Set("Content-Type", "application/json")
	w.Header().Set("Cache-Control", "no-store")
	_ = json.MarshalWrite(w, struct {
		Bytes int64 `json:"bytes"`
		Nanos int64 `json:"nanos"`
	}{agg.bytes.Load(), agg.elapsedNanos(monoNanos())})
}
