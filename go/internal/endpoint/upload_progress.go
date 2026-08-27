package endpoint

import (
	"context"
	"encoding/json/jsontext"
	"encoding/json/v2"
	"io"
	"net/http"
	"net/netip"
	"time"

	"github.com/zR-JB/graphite-meter/go/internal/transport"
)

// UploadProgress streams the selected throughput target's authoritative upload counter as NDJSON.
type UploadProgress struct {
	store   *UploadStore
	trusted []netip.Prefix
}

// NewUploadProgress builds the progress endpoint over store.
func NewUploadProgress(store *UploadStore, trusted ...[]netip.Prefix) *UploadProgress {
	return &UploadProgress{store: store, trusted: optionalPrefixes(trusted)}
}

func (e *UploadProgress) ID() string { return "upload-progress" }

const (
	uploadProgressTick      = 100 * time.Millisecond
	uploadProgressHeartbeat = time.Second
)

type uploadProgressEvent struct {
	Type    string `json:"type"`
	Bytes   uint64 `json:"bytes,omitzero"`
	Nanos   uint64 `json:"nanos,omitzero"`
	Message string `json:"message,omitempty"`
	Code    string `json:"code,omitempty"`
}

func waitForUploadPosts(done, superseded <-chan struct{}, agg *uploadAgg) bool {
	for {
		// Register before reading the count: a lane finishing in between still closes this exact channel.
		changed := agg.postsWaiter()
		if agg.posts.Load() == 0 {
			return true
		}
		select {
		case <-done:
			return false
		case <-superseded:
			return false
		case <-changed:
		}
	}
}

func (e *UploadProgress) Handle(s transport.Session) error {
	w, r, ok := s.HTTP()
	if !ok {
		return transport.ErrUnsupported
	}
	id := r.URL.Query().Get("id")
	// This request-shaped route derives its owner from the HTTP request key.
	owner := ClientKey(r, e.trusted)
	if r.Method == http.MethodDelete {
		if access := e.store.finishFor(id, owner); access != uploadAccessOK {
			writeUploadAccessError(w, access)
			return nil
		}
		w.WriteHeader(http.StatusNoContent)
		return nil
	}
	if r.Method != http.MethodGet {
		w.WriteHeader(http.StatusMethodNotAllowed)
		return nil
	}
	// no-transform and X-Accel-Buffering tell intermediaries not to buffer or recode the stream.
	w.Header().Set("Content-Type", "application/x-ndjson")
	w.Header().Set("Cache-Control", "no-store, no-transform")
	w.Header().Set("X-Accel-Buffering", "no")
	flusher, ok := w.(http.Flusher)
	if !ok {
		http.Error(w, "streaming unsupported", http.StatusInternalServerError)
		return nil
	}
	// NDJSON requires a stateful encoder and one newline-delimited record per event.
	enc := jsontext.NewEncoder(w)
	emit := func(event uploadProgressEvent) bool {
		if err := json.MarshalEncode(enc, event); err != nil {
			return false
		}
		flusher.Flush()
		return true
	}

	e.streamProgress(r.Context().Done(), id, owner, emit, func() bool {
		if _, err := w.Write([]byte("\n")); err != nil {
			return false
		}
		flusher.Flush()
		return true
	}, func(access uploadAccess) { writeUploadAccessError(w, access) })
	return nil
}

// HandleStream serves the feed over a server-opened WebTransport stream.
func (e *UploadProgress) HandleStream(ctx context.Context, id, owner string, w io.Writer) {
	// This WebTransport feed is also NDJSON; retain Encoder framing per record.
	enc := jsontext.NewEncoder(w)
	emit := func(event uploadProgressEvent) bool { return json.MarshalEncode(enc, event) == nil }

	e.streamProgress(ctx.Done(), id, owner, emit, func() bool {
		_, err := w.Write([]byte("\n"))
		return err == nil
	}, func(access uploadAccess) {
		emit(uploadProgressEvent{Type: "error", Message: uploadAccessMessage(access), Code: uploadAccessCode(access)})
	})
}

// streamProgress owns the shared aggregate claim and lifecycle for both feed transports.
func (e *UploadProgress) streamProgress(done <-chan struct{}, id, owner string, emit func(uploadProgressEvent) bool, heartbeat func() bool, refused func(uploadAccess)) {
	// Watching is not upload activity: a progress stream must never refresh the idle clock.
	agg, access := e.store.getOrCreateForActivity(id, owner, false)
	if access != uploadAccessOK {
		refused(access)
		return
	}
	claim := agg.claimProgress()
	defer agg.releaseProgress(claim)
	if !emit(uploadProgressEvent{Type: "ready"}) {
		return
	}
	runProgress(done, claim, agg, emit, heartbeat)
}

func runProgress(done, superseded <-chan struct{}, agg *uploadAgg, emit func(uploadProgressEvent) bool, heartbeat func() bool) {
	tick := time.Tick(uploadProgressTick)
	beat := time.Tick(uploadProgressHeartbeat)
	var lastBytes uint64
	for {
		select {
		case <-done:
			return
		case <-superseded:
			return
		case <-agg.expired:
			return
		case <-agg.finished:
			if !waitForUploadPosts(done, superseded, agg) {
				return
			}
			n := uint64(agg.bytes.Load())                    //nosec G115 -- byte count is non-negative
			elapsed := uint64(agg.elapsedNanos(monoNanos())) //nosec G115 -- elapsed nanos is non-negative
			emit(uploadProgressEvent{Type: "complete", Bytes: n, Nanos: elapsed})
			return
		case <-beat:
			if !heartbeat() {
				return
			}
		case <-tick:
			n := uint64(agg.bytes.Load())                    //nosec G115 -- byte count is non-negative
			elapsed := uint64(agg.elapsedNanos(monoNanos())) //nosec G115 -- elapsed nanos is non-negative
			if n != lastBytes {
				lastBytes = n
				if !emit(uploadProgressEvent{Type: "progress", Bytes: n, Nanos: elapsed}) {
					return
				}
			}
		}
	}
}
