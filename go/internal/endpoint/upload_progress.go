package endpoint

import (
	"encoding/json"
	"net/http"
	"time"

	"github.com/zR-JB/graphite-meter/go/internal/transport"
)

// UploadProgress streams the selected throughput target's authoritative upload
// counter as NDJSON. The initial ready record is flushed before upload lanes are
// allowed to start; blank lines are heartbeats and never carry measurement data.
type UploadProgress struct{ store *UploadStore }

func NewUploadProgress(store *UploadStore) *UploadProgress { return &UploadProgress{store: store} }
func (e *UploadProgress) ID() string                       { return "upload-progress" }
func (e *UploadProgress) Capabilities() Capabilities       { return Capabilities{HTTP: true} }

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

func waitForUploadPosts(ctx <-chan struct{}, agg *uploadAgg) bool {
	for agg.posts.Load() > 0 {
		select {
		case <-ctx:
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
	if r.Method == http.MethodDelete {
		if !e.store.finish(id) {
			http.Error(w, "unknown upload id", http.StatusNotFound)
			return nil
		}
		w.WriteHeader(http.StatusNoContent)
		return nil
	}
	if r.Method != http.MethodGet {
		w.WriteHeader(http.StatusMethodNotAllowed)
		return nil
	}
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

	agg, exists := e.store.getOrCreate(id)
	if !exists {
		w.WriteHeader(http.StatusBadRequest)
		emit(uploadProgressEvent{Type: "error", Message: "unknown upload id"})
		return nil
	}
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
		case <-agg.finished:
			if !waitForUploadPosts(r.Context().Done(), agg) {
				return nil
			}
			n := uint64(agg.bytes.Load())
			elapsed := uint64(agg.elapsedNanos(monoNanos()))
			emit(uploadProgressEvent{Type: "complete", Bytes: n, Nanos: elapsed})
			return nil
		case <-heartbeat.C:
			if _, err := w.Write([]byte("\n")); err != nil {
				return nil
			}
			flusher.Flush()
		case <-tick.C:
			agg.lastTouchMono.Store(monoNanos())
			n := uint64(agg.bytes.Load())
			elapsed := uint64(agg.elapsedNanos(monoNanos()))
			if n != lastBytes {
				lastBytes = n
				if !emit(uploadProgressEvent{Type: "progress", Bytes: n, Nanos: elapsed}) {
					return nil
				}
			}
		}
	}
}
