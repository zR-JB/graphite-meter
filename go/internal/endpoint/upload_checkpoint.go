package endpoint

import (
	"encoding/json/v2"
	"net/http"
	"net/netip"
)

// UploadCheckpoint observes an existing receiver aggregate without keeping it alive.
type UploadCheckpoint struct {
	store   *UploadStore
	trusted []netip.Prefix
}

func NewUploadCheckpoint(store *UploadStore, trusted []netip.Prefix) *UploadCheckpoint {
	return &UploadCheckpoint{store: store, trusted: trusted}
}

func (e *UploadCheckpoint) HandleHTTP(w http.ResponseWriter, r *http.Request) error {
	if r.Method != http.MethodPost {
		w.WriteHeader(http.StatusMethodNotAllowed)
		return nil
	}
	agg, found := e.store.get(r.URL.Query().Get("id"))
	if !found {
		writeUploadAccessError(w, uploadAccessInvalid)
		return nil
	}
	if agg.owner != "" && agg.owner != UploadOwner(r, e.trusted) {
		writeUploadAccessError(w, uploadAccessOwnerMismatch)
		return nil
	}
	w.Header().Set("Content-Type", "application/json")
	w.Header().Set("Cache-Control", "no-store")
	return json.MarshalWrite(w, struct {
		Bytes int64 `json:"bytes"`
		Nanos int64 `json:"nanos"`
	}{agg.bytes.Load(), agg.elapsedNanos(monoNanos())})
}
