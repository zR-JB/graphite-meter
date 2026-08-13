package endpoint

import (
	"io"
	"net/http"
	"net/netip"
	"strconv"
	"sync"
	"time"

	"github.com/zR-JB/graphite-meter/go/internal/transport"
)

// Upload sinks the client's streamed bytes for the upload measurement, draining
// the body through a pooled scratch buffer without accumulating it. A server-
// minted ?id= folds every chunk into that test's shared aggregate, so the
// SERVER's drained count is authoritative and readable from /upload/progress.
type Upload struct {
	meter   *Meter       // optional verbose per-second logger; nil unless -verbose
	store   *UploadStore // optional per-id aggregate; nil disables server-authoritative counting
	trusted []netip.Prefix
}

// uploadReadTimeout bounds a single stuck POST's body read so a half-open lane
// cannot pin a goroutine indefinitely. Generous against a normal 10 to 20 s
// upload stage; each POST on a keep-alive connection gets a fresh deadline.
const uploadReadTimeout = 120 * time.Second

// NewUpload builds the upload endpoint. meter may be nil (no verbose logging);
// store may be nil (no server-authoritative per-id counting). The optional
// trusted prefixes are the proxies whose forwarded-for headers may be believed
// when attributing an upload to a client.
func NewUpload(meter *Meter, store *UploadStore, trusted ...[]netip.Prefix) *Upload {
	u := &Upload{meter: meter, store: store}
	if len(trusted) > 0 {
		u.trusted = trusted[0]
	}
	return u
}

func (u *Upload) ID() string { return "upload" }

// uploadBufSize is the drain buffer per in-flight upload. Larger than
// io.Discard's internal 8 KiB so a saturated link costs far fewer read syscalls.
const uploadBufSize = 256 * 1024

// scratchPool reuses drain buffers across uploads for zero per-request
// allocation. It stores *[]byte so Get/Put do not box the slice header.
var scratchPool = sync.Pool{
	New: func() any {
		b := make([]byte, uploadBufSize)
		return &b
	},
}

// discardSink counts bytes via io.Copy's return value and drops them. It must
// NOT implement io.ReaderFrom: that makes io.CopyBuffer bypass the large pooled
// buffer for io.Discard's small internal one. An attached meter or aggregate
// sees every chunk.
type discardSink struct {
	meter *Meter
	agg   *uploadAgg // nil unless the POST carried a valid server-minted ?id=
}

func (s discardSink) Write(p []byte) (int, error) {
	s.meter.Add(len(p))
	if s.agg != nil {
		// The one upload counting point: the first drained chunk anchors the server
		// elapsed clock and every drained byte is counted exactly once.
		s.agg.recordChunk(monoNanos(), len(p))
	}
	return len(p), nil
}

// Handle drains the upload source, counting bytes. A clean EOF echoes the count
// as JSON; a mid-stream cancel (client aborted the measurement) stops quietly.
func (u *Upload) Handle(s transport.Session) error {
	// A server-minted ?id= joins this POST to its test's shared aggregate. Without
	// a valid one the POST still drains and counts, just not authoritatively.
	var agg *uploadAgg
	if u.store != nil {
		id := s.Query().Get("id")
		if id != "" {
			owner := sessionOwner(s, u.trusted)
			a, access := u.store.getOrCreateFor(id, owner)
			if access != uploadAccessOK {
				w, _, ok := s.HTTP()
				if !ok {
					// A stream carries no status line, so the refusal is the
					// return value: its caller resets the stream rather than
					// leaving the client parked on flow control.
					return &uploadRefusalError{access: access}
				}
				writeUploadAccessError(w, access)
				return nil
			}
			agg = a
			agg.changePosts(1)
			defer agg.changePosts(-1)
		}
	}

	src, err := s.OpenUploadSource()
	if err != nil {
		return err
	}

	// Per-request deadline: the streaming server sets no global ReadTimeout, which
	// would cut long legitimate uploads. A writer without deadlines runs unbounded.
	if w, _, ok := s.HTTP(); ok {
		_ = http.NewResponseController(w).SetReadDeadline(time.Now().Add(uploadReadTimeout))
	}

	bufp := scratchPool.Get().(*[]byte)
	defer scratchPool.Put(bufp)

	u.meter.Open()
	defer u.meter.Close()

	// The http server cancels the body read on request-context cancellation, so
	// CopyBuffer returns promptly when the client disconnects.
	n, copyErr := io.CopyBuffer(discardSink{meter: u.meter, agg: agg}, src, *bufp)
	if copyErr != nil {
		// The client aborted the stream, the common case here. The connection is
		// gone, so there is nothing to reply to.
		return nil
	}

	if w, _, ok := s.HTTP(); ok {
		h := w.Header()
		h.Set("Content-Type", "application/json")
		h.Set("Cache-Control", "no-store")
		// The body is the last thing written; a failure here means the client is
		// already gone and there is nowhere left to report it.
		_, _ = io.WriteString(w, `{"bytes":`+strconv.FormatInt(n, 10)+`}`)
	}
	return nil
}
