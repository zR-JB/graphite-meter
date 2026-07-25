package endpoint

import (
	"context"
	"encoding/json"
	"io"
	"net/http"
	"net/netip"
	"time"

	"github.com/zR-JB/graphite-meter/go/internal/transport"
)

// UploadProgress streams the selected throughput target's authoritative upload
// counter as NDJSON. The ready record flushes first, then upload lanes may
// start. Blank lines are heartbeats and never carry measurement data.
type UploadProgress struct {
	store   *UploadStore
	trusted []netip.Prefix
}

// NewUploadProgress builds the progress endpoint over store. The optional trusted
// prefixes are the proxies whose forwarded-for headers may be believed when
// checking that a stream's caller owns the upload it asks about.
func NewUploadProgress(store *UploadStore, trusted ...[]netip.Prefix) *UploadProgress {
	e := &UploadProgress{store: store}
	if len(trusted) > 0 {
		e.trusted = trusted[0]
	}
	return e
}

func (e *UploadProgress) ID() string                 { return "upload-progress" }
func (e *UploadProgress) Capabilities() Capabilities { return Capabilities{HTTP: true} }

const (
	uploadProgressTick      = 100 * time.Millisecond
	uploadProgressHeartbeat = time.Second
)

type uploadProgressEvent struct {
	Type    string `json:"type"`
	Bytes   uint64 `json:"bytes,omitempty"`
	Nanos   uint64 `json:"nanos,omitempty"`
	Message string `json:"message,omitempty"`
}

// waitForUploadPosts blocks until no POST lane is still draining into agg, so the
// terminal count includes every in-flight lane rather than racing them. It
// reports false if done or superseded fires first, meaning this feed has no one
// left to report the total to.
func waitForUploadPosts(done, superseded <-chan struct{}, agg *uploadAgg) bool {
	for {
		// Register before reading the count: a lane finishing in between still
		// closes this exact channel.
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
	// no-transform and X-Accel-Buffering tell intermediaries not to buffer or
	// recode the stream; a proxy holding records back would stall the progress UI.
	w.Header().Set("Content-Type", "application/x-ndjson")
	w.Header().Set("Cache-Control", "no-store, no-transform")
	w.Header().Set("X-Accel-Buffering", "no")
	flusher, ok := w.(http.Flusher)
	if !ok {
		http.Error(w, "streaming unsupported", http.StatusInternalServerError)
		return nil
	}
	enc := json.NewEncoder(w)
	emit := func(event uploadProgressEvent) bool {
		if err := enc.Encode(event); err != nil {
			return false
		}
		flusher.Flush()
		return true
	}

	// Watching is not upload activity: a progress stream must never refresh the
	// idle clock, or a client that stopped uploading would keep its slot forever.
	agg, access := e.store.getOrCreateForActivity(id, owner, false)
	if access != uploadAccessOK {
		writeUploadAccessError(w, access)
		return nil
	}
	claim := agg.claimProgress()
	defer agg.releaseProgress(claim)
	if !emit(uploadProgressEvent{Type: "ready"}) {
		return nil
	}
	runProgress(r.Context().Done(), claim, agg, emit, func() bool {
		if _, err := w.Write([]byte("\n")); err != nil {
			return false
		}
		flusher.Flush()
		return true
	})
	return nil
}

// HandleStream serves the same feed over a byte stream, the unidirectional
// stream the server opens on a WebTransport upload session. Stream writes are
// unbuffered, so there is no flush step.
func (e *UploadProgress) HandleStream(ctx context.Context, id, owner string, w io.Writer) {
	enc := json.NewEncoder(w)
	emit := func(event uploadProgressEvent) bool { return enc.Encode(event) == nil }

	agg, access := e.store.getOrCreateForActivity(id, owner, false)
	if access != uploadAccessOK {
		emit(uploadProgressEvent{Type: "error", Message: uploadAccessMessage(access)})
		return
	}
	claim := agg.claimProgress()
	defer agg.releaseProgress(claim)
	if !emit(uploadProgressEvent{Type: "ready"}) {
		return
	}
	runProgress(ctx.Done(), claim, agg, emit, func() bool {
		_, err := w.Write([]byte("\n"))
		return err == nil
	})
}

// runProgress reports agg's counter until the upload completes, expires, done
// fires, or a newer feed supersedes this one. emit and heartbeat report false
// once their sink is gone.
func runProgress(done, superseded <-chan struct{}, agg *uploadAgg, emit func(uploadProgressEvent) bool, heartbeat func() bool) {
	tick := time.NewTicker(uploadProgressTick)
	defer tick.Stop()
	beat := time.NewTicker(uploadProgressHeartbeat)
	defer beat.Stop()
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
		case <-beat.C:
			if !heartbeat() {
				return
			}
		case <-tick.C:
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
