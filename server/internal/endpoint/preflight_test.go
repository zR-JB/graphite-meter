package endpoint

import (
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"testing"

	"github.com/zR-JB/graphite-meter/server/internal/config"
	"github.com/zR-JB/graphite-meter/server/internal/wire"
)

// TestPreflightMintsUploadID checks that /preflight issues a fresh, unique upload
// id per call, marks it issued in the store, and advertises the /ws/upload path —
// the correlation handshake the server-authoritative upload depends on.
func TestPreflightMintsUploadID(t *testing.T) {
	store := NewUploadStore()
	cfg := config.Default()
	mux := http.NewServeMux()
	mux.Handle("/preflight", httpAdapter(NewPreflight(&cfg, store)))
	srv := httptest.NewServer(mux)
	defer srv.Close()

	get := func() wire.Preflight {
		res, err := http.Get(srv.URL + "/preflight")
		if err != nil {
			t.Fatalf("get: %v", err)
		}
		defer res.Body.Close()
		var pf wire.Preflight
		if err := json.NewDecoder(res.Body).Decode(&pf); err != nil {
			t.Fatalf("decode: %v", err)
		}
		return pf
	}

	a, b := get(), get()

	if a.UploadID == "" || b.UploadID == "" {
		t.Fatalf("uploadId missing: a=%q b=%q", a.UploadID, b.UploadID)
	}
	if a.UploadID == b.UploadID {
		t.Errorf("uploadId not unique across calls: %q", a.UploadID)
	}
	if !store.isIssued(a.UploadID) || !store.isIssued(b.UploadID) {
		t.Errorf("minted ids not marked issued in the store")
	}
	if a.Capabilities.Endpoints.WSUpload != "/ws/upload" {
		t.Errorf("wsUpload = %q, want /ws/upload", a.Capabilities.Endpoints.WSUpload)
	}
}

// TestPreflightNilStoreOmitsUploadID checks the graceful path: with no store wired,
// no id is minted and the field is omitted (client then self-counts the upload).
func TestPreflightNilStoreOmitsUploadID(t *testing.T) {
	cfg := config.Default()
	mux := http.NewServeMux()
	mux.Handle("/preflight", httpAdapter(NewPreflight(&cfg, nil)))
	srv := httptest.NewServer(mux)
	defer srv.Close()

	res, err := http.Get(srv.URL + "/preflight")
	if err != nil {
		t.Fatalf("get: %v", err)
	}
	defer res.Body.Close()
	var raw map[string]any
	if err := json.NewDecoder(res.Body).Decode(&raw); err != nil {
		t.Fatalf("decode: %v", err)
	}
	if _, present := raw["uploadId"]; present {
		t.Errorf("uploadId present with a nil store, want omitted")
	}
}
