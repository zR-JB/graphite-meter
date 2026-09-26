package endpoint

import (
	"context"
	"encoding/json/jsontext"
	"encoding/json/v2"
	"io"
	"net/http"
	"time"

	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

const (
	uploadProgressTick      = 100 * time.Millisecond
	uploadProgressHeartbeat = time.Second
)

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

// ServeProgress streams the receiver's counter as NDJSON on GET and finishes it on DELETE.
func (u *Upload) ServeProgress(w http.ResponseWriter, r *http.Request) {
	id := r.URL.Query().Get("id")
	// This request-shaped route derives its owner from the HTTP request key.
	owner := UploadOwner(r, u.trusted)
	switch r.Method {
	case http.MethodDelete:
		if access := u.store.finishFor(id, owner); access != uploadAccessOK {
			writeUploadAccessError(w, access)
			return
		}
		w.WriteHeader(http.StatusNoContent)
		return
	case http.MethodHead:
		// GET's route also carries HEAD, which must not claim a feed.
		w.Header().Set("Allow", "GET, DELETE")
		w.WriteHeader(http.StatusMethodNotAllowed)
		return
	}
	flusher, ok := w.(http.Flusher)
	if !ok {
		http.Error(w, "streaming unsupported", http.StatusInternalServerError)
		return
	}
	agg, access := u.store.watchFor(id, owner)
	if access != uploadAccessOK {
		writeUploadAccessError(w, access)
		return
	}
	// no-transform and X-Accel-Buffering tell intermediaries not to buffer or recode the stream.
	w.Header().Set("Content-Type", "application/x-ndjson")
	w.Header().Set("Cache-Control", "no-store, no-transform")
	w.Header().Set("X-Accel-Buffering", "no")
	// NDJSON requires a stateful encoder and one newline-delimited record per event.
	enc := jsontext.NewEncoder(w)
	serveProgress(r.Context().Done(), agg, u.store.now, func(event wire.UploadProgress) bool {
		if err := json.MarshalEncode(enc, event); err != nil {
			return false
		}
		flusher.Flush()
		return true
	}, func() bool {
		if _, err := w.Write([]byte("\n")); err != nil {
			return false
		}
		flusher.Flush()
		return true
	})
}

// streamProgress serves a receiver's feed on a WebTransport stream.
func streamProgress(ctx context.Context, agg *uploadAgg, now func() int64, w io.Writer) {
	// This WebTransport feed is also NDJSON; retain Encoder framing per record.
	enc := jsontext.NewEncoder(w)
	serveProgress(ctx.Done(), agg, now, func(event wire.UploadProgress) bool { return json.MarshalEncode(enc, event) == nil }, func() bool {
		_, err := w.Write([]byte("\n"))
		return err == nil
	})
}

// writeRefusalRecord is the refusal a stream carries in place of a status line.
func writeRefusalRecord(w io.Writer, access uploadAccess) {
	_ = json.MarshalEncode(jsontext.NewEncoder(w), wire.UploadProgress{Type: "error", Message: uploadAccessMessage(access), Code: uploadAccessCode(access)})
}

// serveProgress owns the feed's claim and lifecycle for both transports.
func serveProgress(done <-chan struct{}, agg *uploadAgg, now func() int64, emit func(wire.UploadProgress) bool, heartbeat func() bool) {
	claim := agg.claimProgress()
	defer agg.releaseProgress(claim)
	if !emit(wire.UploadProgress{Type: "ready"}) {
		return
	}
	runProgress(done, claim, agg, now, emit, heartbeat)
}

func runProgress(done, superseded <-chan struct{}, agg *uploadAgg, now func() int64, emit func(wire.UploadProgress) bool, heartbeat func() bool) {
	tick := time.Tick(uploadProgressTick)
	beat := time.Tick(uploadProgressHeartbeat)
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
			n := uint64(agg.bytes.Load())              //nosec G115 -- byte count is non-negative
			elapsed := uint64(agg.elapsedNanos(now())) //nosec G115 -- elapsed nanos is non-negative
			emit(wire.UploadProgress{Type: "complete", Bytes: n, Nanos: elapsed})
			return
		case <-beat:
			if !heartbeat() {
				return
			}
		case <-tick:
			n := uint64(agg.bytes.Load())              //nosec G115 -- byte count is non-negative
			elapsed := uint64(agg.elapsedNanos(now())) //nosec G115 -- elapsed nanos is non-negative
			if elapsed > 0 {
				if !emit(wire.UploadProgress{Type: "progress", Bytes: n, Nanos: elapsed}) {
					return
				}
			}
		}
	}
}
