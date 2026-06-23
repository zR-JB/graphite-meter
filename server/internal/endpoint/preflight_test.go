package endpoint

import (
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"testing"

	"github.com/zR-JB/graphite-meter/server/internal/config"
	"github.com/zR-JB/graphite-meter/server/internal/wire"
)

// TestPreflightAdvertisesUploadSession checks that /preflight advertises the
// upload warmup token endpoint without minting a token globally.
func TestPreflightAdvertisesUploadSession(t *testing.T) {
	cfg := config.Default()
	mux := http.NewServeMux()
	mux.Handle("/preflight", httpAdapter(NewPreflight(&cfg)))
	srv := httptest.NewServer(mux)
	defer srv.Close()

	res, err := http.Get(srv.URL + "/preflight")
	if err != nil {
		t.Fatalf("get: %v", err)
	}
	defer res.Body.Close()
	var raw map[string]any
	if err := json.NewDecoder(res.Body).Decode(&raw); err != nil {
		t.Fatalf("decode raw: %v", err)
	}
	if _, present := raw["uploadId"]; present {
		t.Errorf("uploadId present in preflight, want omitted")
	}
	var pf wire.Preflight
	body, _ := json.Marshal(raw)
	if err := json.Unmarshal(body, &pf); err != nil {
		t.Fatalf("decode struct: %v", err)
	}
	if pf.Capabilities.Endpoints.UploadSession != "/upload/session" {
		t.Errorf("uploadSession = %q, want /upload/session", pf.Capabilities.Endpoints.UploadSession)
	}
	if pf.Capabilities.Endpoints.WSUpload != "/ws/upload" {
		t.Errorf("wsUpload = %q, want /ws/upload", pf.Capabilities.Endpoints.WSUpload)
	}
}
