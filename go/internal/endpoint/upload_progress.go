package endpoint

import (
	"encoding/json"
	"net/http"
	"net/netip"
	"time"

	"github.com/zR-JB/graphite-meter/go/internal/transport"
)

// UploadProgress streams the selected throughput target's authoritative upload
// counter as NDJSON. The initial ready record is flushed before upload lanes are
// allowed to start; blank lines are heartbeats and never carry measurement data.
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
// reports false if done fires first, meaning the client left and there is no one
// to report the total to.
func waitForUploadPosts(done <-chan struct{}, agg *uploadAgg) bool {
	for agg.posts.Load() > 0 {
		select {
		case <-done:
			return false
		case <-agg.postsChanged:
		}
	}
	return true
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
	if !agg.progressActive.CompareAndSwap(false, true) {
		http.Error(w, "upload progress already connected", http.StatusConflict)
		return nil
	}
	defer agg.progressActive.Store(false)
	if !emit(uploadProgressEvent{Type: "ready"}) {
		return nil
	}

	tick := time.NewTicker(uploadProgressTick)
	defer tick.Stop()
	heartbeat := time.NewTicker(uploadProgressHeartbeat)
	defer heartbeat.Stop()
	var lastBytes uint64
	for {
		select {
		case <-r.Context().Done():
			return nil
		case <-agg.expired:
			return nil
		case <-agg.finished:
			if !waitForUploadPosts(r.Context().Done(), agg) {
				return nil
			}
			n := uint64(agg.bytes.Load())                    //nosec G115 -- byte count is non-negative
			elapsed := uint64(agg.elapsedNanos(monoNanos())) //nosec G115 -- elapsed nanos is non-negative
			emit(uploadProgressEvent{Type: "complete", Bytes: n, Nanos: elapsed})
			return nil
		case <-heartbeat.C:
			if _, err := w.Write([]byte("\n")); err != nil {
				return nil
			}
			flusher.Flush()
		case <-tick.C:
			n := uint64(agg.bytes.Load())                    //nosec G115 -- byte count is non-negative
			elapsed := uint64(agg.elapsedNanos(monoNanos())) //nosec G115 -- elapsed nanos is non-negative
			if n != lastBytes {
				lastBytes = n
				if !emit(uploadProgressEvent{Type: "progress", Bytes: n, Nanos: elapsed}) {
					return nil
				}
			}
		}
	}
}
