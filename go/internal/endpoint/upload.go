package endpoint

import (
	"io"
	"net/http"
	"strconv"
	"sync"
	"time"

	"github.com/zR-JB/graphite-meter/go/internal/transport"
)

// Upload sinks the client's streamed bytes for the upload measurement: it drains
// the request body to io.Discard through a pooled scratch buffer, counting but
// never accumulating (zero allocation, no buffering).
//
// When the request carries a server-minted ?id=, each drained chunk is also added
// to that test's shared per-id aggregate (across all its parallel POST lanes), so
// the SERVER's drained count — not the browser's upload.onprogress — becomes the
// authoritative upload result, read live by the /ws/upload progress bus. Without
// an id it behaves exactly as before (client self-counts).
type Upload struct {
	meter *Meter       // optional verbose per-second logger; nil unless -verbose
	store *UploadStore // optional per-id aggregate; nil disables server-authoritative counting
}

// uploadReadTimeout bounds a single stuck POST's body read so a half-open lane
// can't pin a goroutine indefinitely. Generous vs a normal ~10–20 s upload stage;
// each POST on a keep-alive connection gets its own fresh deadline.
const uploadReadTimeout = 120 * time.Second

// NewUpload builds the upload endpoint. meter may be nil (no verbose logging);
// store may be nil (no server-authoritative per-id counting).
func NewUpload(meter *Meter, store *UploadStore) *Upload {
	return &Upload{meter: meter, store: store}
}

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
// large pooled buffer instead of io.Discard's small internal one. When verbose,
// it also feeds each drained chunk to the meter for live per-second logging; when
// an aggregate is attached, it folds the chunk into the test's server-
// authoritative per-id count and active-time clock.
type discardSink struct {
	meter *Meter
	agg   *uploadAgg // nil unless the POST carried a valid server-minted ?id=
}

func (s discardSink) Write(p []byte) (int, error) {
	s.meter.Add(len(p))
	if s.agg != nil {
		// The ONE upload counting point: bytes AND the active measurement clock are
		// folded in together here, at the drain, so the rate the client computes
		// (Δbytes / ΔactiveNanos) is measured at a single point with no double count.
		s.agg.recordChunk(monoNanos(), len(p))
	}
	return len(p), nil
}

// Handle drains the upload source, counting bytes. A clean EOF echoes the count
// as JSON; a mid-stream cancel (client aborted the measurement) stops quietly.
func (u *Upload) Handle(s transport.Session) error {
	src, err := s.OpenUploadSource()
	if err != nil {
		return err
	}

	// Resolve the test's shared aggregate from a server-minted ?id=; nil for an
	// empty/unissued id or over the live cap, in which case this POST still
	// drains and counts, just not server-authoritatively. posts is a
	// diagnostics gauge only — the TTL sweeper owns deletion.
	var agg *uploadAgg
	if u.store != nil {
		if a, ok := u.store.getOrCreate(s.Query().Get("id")); ok {
			agg = a
			agg.posts.Add(1)
			defer agg.posts.Add(-1)
		}
	}

	// Bound a single stuck POST's body read (idiomatic per-request deadline via the
	// ResponseController). The streaming download/upload server has no global
	// ReadTimeout — that would kill long legit uploads — so we scope it per POST.
	if w, _, ok := s.HTTP(); ok {
		_ = http.NewResponseController(w).SetReadDeadline(time.Now().Add(uploadReadTimeout))
	}

	bufp := scratchPool.Get().(*[]byte)
	defer scratchPool.Put(bufp)

	u.meter.Open()
	defer u.meter.Close()

	// CopyBuffer reads until EOF or a read error; the http server cancels the
	// body read when the request context is cancelled, so this returns promptly
	// on client disconnect.
	n, copyErr := io.CopyBuffer(discardSink{meter: u.meter, agg: agg}, src, *bufp)
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
