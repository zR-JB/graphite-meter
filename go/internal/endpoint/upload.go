package endpoint

import (
	"context"
	"crypto/rand"
	"crypto/sha256"
	"encoding/json/jsontext"
	"encoding/json/v2"
	"errors"
	"io"
	"net/http"
	"net/netip"
	"os"
	"strconv"
	"sync"
	"time"

	"github.com/zR-JB/graphite-meter/go/internal/auth"
	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

// Upload is the receiver store and its routes; receivers own the uploaded bytes and time.
type Upload struct {
	meter     *Meter // optional verbose per-second logger; nil unless -verbose
	trusted   []netip.Prefix
	epoch     time.Time
	tokenKey  [sha256.Size]byte
	mu        sync.Mutex
	receivers map[string]*uploadAgg
	evicted   map[string]int64
	byClient  map[string]int
}

const (
	// Reads rarely exceed a socket or stream buffer; a larger one only costs memory.
	uploadBufSize           = 64 * 1024
	uploadProgressTick      = 100 * time.Millisecond
	uploadProgressHeartbeat = time.Second
)

func NewUpload(meter *Meter, trusted []netip.Prefix) *Upload {
	u := &Upload{meter: meter, trusted: trusted, epoch: time.Now(), receivers: map[string]*uploadAgg{},
		evicted: map[string]int64{}, byClient: map[string]int{}}
	_, _ = rand.Read(u.tokenKey[:])
	return u
}

var scratchPool = sync.Pool{New: func() any { return new(make([]byte, uploadBufSize)) }}

func (u *Upload) Handler(idle time.Duration) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) { u.serve(w, r, idle) })
}

func (u *Upload) serve(w http.ResponseWriter, r *http.Request, bound time.Duration) {
	limit, _ := r.Context().Deadline()
	idle := &idleDeadline{set: http.NewResponseController(w).SetReadDeadline, bound: bound, limit: limit}
	defer idle.endWith(r.Context())()
	n, err := u.Receive(r.URL.Query().Get("id"), uploadClientOf(r, u.trusted), r.Body, idle)
	if refusal, ok := errors.AsType[*uploadRefusalError](err); ok {
		writeUploadAccessError(w, refusal.access)
		return
	}
	switch {
	case err != nil && auth.SessionEnded(r.Context()):
		auth.SignInRequired(w.Header())
		w.Header().Set("X-Graphite-Upload-Refusal", wire.LaneRevoked.Name)
		http.Error(w, wire.LaneRevoked.Reason, http.StatusForbidden)
		return
	case errors.Is(err, os.ErrDeadlineExceeded) && (limit.IsZero() || time.Now().Before(limit)):
		w.Header().Set("X-Graphite-Upload-Refusal", wire.LaneIdle.Name)
		http.Error(w, wire.LaneIdle.Reason, http.StatusRequestTimeout)
		return
	case err != nil:
		return
	}
	noStoreJSON(w)
	_, _ = io.WriteString(w, `{"bytes":`+strconv.FormatInt(n, 10)+`}`)
}

// Receive joins the owner's receiver before reading and records each chunk as the lane's progress.
func (u *Upload) Receive(id string, c uploadClient, src io.Reader, idle *idleDeadline) (int64, error) {
	agg, access := u.accessFor(id, c, true)
	if access != uploadAccessOK {
		return 0, &uploadRefusalError{access: access}
	}
	defer u.leave(agg)
	bufp := scratchPool.Get().(*[]byte)
	defer scratchPool.Put(bufp)
	u.meter.Open()
	defer u.meter.Close()
	idle.moved(time.Now())
	var total int64
	for {
		n, err := src.Read(*bufp)
		if n > 0 {
			now := time.Now()
			idle.moved(now)
			u.meter.Add(n)
			agg.recordChunk(u.mono(now), n)
			total += int64(n)
		}
		if err == io.EOF {
			return total, nil
		}
		if err != nil {
			return total, err
		}
	}
}

func (u *Upload) ServeSession(w http.ResponseWriter, _ *http.Request) {
	noStoreJSON(w)
	_ = json.MarshalWrite(w, struct {
		UploadID string `json:"uploadId"`
	}{u.Mint()})
}

// ServeCheckpoint reports an existing receiver's counter without keeping it alive.
func (u *Upload) ServeCheckpoint(w http.ResponseWriter, r *http.Request) {
	agg, found := u.get(r.URL.Query().Get("id"))
	if !found {
		writeUploadAccessError(w, uploadAccessInvalid)
		return
	}
	if agg.client.owner != uploadClientOf(r, u.trusted).owner {
		writeUploadAccessError(w, uploadAccessOwnerMismatch)
		return
	}
	noStoreJSON(w)
	_ = json.MarshalWrite(w, struct {
		Bytes int64 `json:"bytes"`
		Nanos int64 `json:"nanos"`
	}{agg.bytes.Load(), agg.elapsedNanos(u.now())})
}

// ServeProgress streams the receiver's counter as NDJSON on GET and finishes it on DELETE.
func (u *Upload) ServeProgress(w http.ResponseWriter, r *http.Request) {
	id, client := r.URL.Query().Get("id"), uploadClientOf(r, u.trusted)
	switch r.Method {
	case http.MethodDelete:
		if access := u.finishFor(id, client.owner); access != uploadAccessOK {
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
	agg, access := u.accessFor(id, client, false)
	if access != uploadAccessOK {
		writeUploadAccessError(w, access)
		return
	}
	w.Header().Set("Content-Type", "application/x-ndjson")
	w.Header().Set("Cache-Control", "no-store, no-transform")
	w.Header().Set("X-Accel-Buffering", "no")
	u.streamProgress(r.Context(), agg, flushWriter{w, flusher})
}

type flushWriter struct {
	io.Writer
	http.Flusher
}

func (w flushWriter) Write(p []byte) (int, error) {
	n, err := w.Writer.Write(p)
	w.Flush()
	return n, err
}

// streamProgress owns the feed's claim and lifecycle: NDJSON records and a bare-newline heartbeat.
func (u *Upload) streamProgress(ctx context.Context, agg *uploadAgg, w io.Writer) {
	enc := jsontext.NewEncoder(w)
	emit := func(event wire.UploadProgress) bool { return json.MarshalEncode(enc, event) == nil }
	claim := u.claimFeed(agg)
	defer u.releaseFeed(agg, claim)
	if !emit(wire.UploadProgress{Type: "ready"}) {
		return
	}
	u.runProgress(ctx.Done(), claim, agg, emit, func() bool {
		_, err := w.Write([]byte("\n"))
		return err == nil
	})
}

func refusalRecord(access uploadAccess) wire.UploadProgress {
	info := uploadAccessInfos[access]
	return wire.UploadProgress{Type: "error", Message: info.message, Code: info.code}
}

func writeRefusalRecord(w io.Writer, access uploadAccess) {
	_ = json.MarshalEncode(jsontext.NewEncoder(w), refusalRecord(access))
}

func (u *Upload) runProgress(done, superseded <-chan struct{}, agg *uploadAgg, emit func(wire.UploadProgress) bool,
	heartbeat func() bool) {
	tick := time.Tick(uploadProgressTick)
	beat := time.Tick(uploadProgressHeartbeat)
	counters := func(kind string) wire.UploadProgress {
		n := uint64(agg.bytes.Load())                //nosec G115 -- byte count is non-negative
		elapsed := uint64(agg.elapsedNanos(u.now())) //nosec G115 -- elapsed nanos is non-negative
		return wire.UploadProgress{Type: kind, Bytes: n, Nanos: elapsed}
	}
	for {
		select {
		case <-done:
			return
		case <-superseded:
			return
		case <-agg.expired:
			emit(refusalRecord(uploadAccessInvalid))
			return
		case <-agg.finished:
			if u.waitDrained(done, superseded, agg) {
				emit(counters("complete"))
			}
			return
		case <-beat:
			if !heartbeat() {
				return
			}
		case <-tick:
			if event := counters("progress"); event.Nanos > 0 && !emit(event) {
				return
			}
		}
	}
}

// waitDrained waits for a finished receiver's last lane, re-registering before each count read.
func (u *Upload) waitDrained(done, superseded <-chan struct{}, agg *uploadAgg) bool {
	for {
		u.mu.Lock()
		if agg.lanes == 0 {
			u.mu.Unlock()
			return true
		}
		if agg.lanesChanged == nil {
			agg.lanesChanged = make(chan struct{})
		}
		changed := agg.lanesChanged
		u.mu.Unlock()
		select {
		case <-done:
			return false
		case <-superseded:
			return false
		case <-changed:
		}
	}
}
