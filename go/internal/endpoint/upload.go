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

// Upload sinks the client's streamed bytes for the upload measurement.
type Upload struct {
	meter   *Meter       // optional verbose per-second logger; nil unless -verbose
	store   *UploadStore // optional per-id aggregate; nil disables server-authoritative counting
	trusted []netip.Prefix
}

// uploadReadTimeout bounds a single stuck POST's body read so a half-open lane cannot pin a goroutine indefinitely.
const uploadReadTimeout = 120 * time.Second

// NewUpload builds the upload endpoint. meter may be nil (no verbose logging).
func NewUpload(meter *Meter, store *UploadStore, trusted ...[]netip.Prefix) *Upload {
	return &Upload{meter: meter, store: store, trusted: optionalPrefixes(trusted)}
}

func (u *Upload) ID() string { return "upload" }

const uploadBufSize = 256 * 1024

var scratchPool = sync.Pool{
	New: func() any {
		return new(make([]byte, uploadBufSize))
	},
}

type discardSink struct {
	meter *Meter
	agg   *uploadAgg // nil unless the POST carried a valid server-minted ?id=
}

func (s discardSink) Write(p []byte) (int, error) {
	s.meter.Add(len(p))
	if s.agg != nil {
		s.agg.recordChunk(monoNanos(), len(p))
	}
	return len(p), nil
}

// Handle drains the upload source, counting bytes.
func (u *Upload) Handle(s transport.Session) error {
	w, _, isHTTP := s.HTTP()
	// A server-minted ?id= joins this POST to its test's shared aggregate.
	var agg *uploadAgg
	if u.store != nil {
		id := s.Query().Get("id")
		if id != "" {
			owner := sessionOwner(s, u.trusted)
			a, access := u.store.getOrCreateFor(id, owner)
			if access != uploadAccessOK {
				if !isHTTP {
					// A stream carries no status line, so the refusal is the return value.
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

	if isHTTP {
		_ = http.NewResponseController(w).SetReadDeadline(time.Now().Add(uploadReadTimeout))
	}

	bufp := scratchPool.Get().(*[]byte)
	defer scratchPool.Put(bufp)

	u.meter.Open()
	defer u.meter.Close()

	n, copyErr := io.CopyBuffer(discardSink{meter: u.meter, agg: agg}, src, *bufp)
	if copyErr != nil {
		// The client aborted the stream, the common case here.
		return nil
	}

	if isHTTP {
		h := w.Header()
		h.Set("Content-Type", "application/json")
		h.Set("Cache-Control", "no-store")
		// A body write failure means the client is gone and there is nowhere to report it.
		_, _ = io.WriteString(w, `{"bytes":`+strconv.FormatInt(n, 10)+`}`)
	}
	return nil
}
