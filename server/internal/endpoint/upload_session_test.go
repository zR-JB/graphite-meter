package endpoint

import (
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"testing"
)

func TestUploadSessionMintsFreshID(t *testing.T) {
	store := NewUploadStore()
	mux := http.NewServeMux()
	mux.Handle("/upload/session", httpAdapter(NewUploadSession(store)))
	srv := httptest.NewServer(mux)
	defer srv.Close()

	mint := func() string {
		res, err := http.Post(srv.URL+"/upload/session", "application/json", nil)
		if err != nil {
			t.Fatalf("post: %v", err)
		}
		defer res.Body.Close()
		if res.StatusCode != http.StatusOK {
			t.Fatalf("status = %d, want 200", res.StatusCode)
		}
		var body struct {
			UploadID string `json:"uploadId"`
		}
		if err := json.NewDecoder(res.Body).Decode(&body); err != nil {
			t.Fatalf("decode: %v", err)
		}
		return body.UploadID
	}

	a, b := mint(), mint()
	if a == "" || b == "" {
		t.Fatalf("uploadId missing: a=%q b=%q", a, b)
	}
	if a == b {
		t.Fatalf("uploadId not unique across calls: %q", a)
	}
	if !store.isIssued(a) || !store.isIssued(b) {
		t.Fatalf("minted ids not marked issued")
	}
}

func TestUploadSessionRejectsGet(t *testing.T) {
	mux := http.NewServeMux()
	mux.Handle("/upload/session", httpAdapter(NewUploadSession(NewUploadStore())))
	srv := httptest.NewServer(mux)
	defer srv.Close()

	res, err := http.Get(srv.URL + "/upload/session")
	if err != nil {
		t.Fatalf("get: %v", err)
	}
	defer res.Body.Close()
	if res.StatusCode != http.StatusMethodNotAllowed {
		t.Fatalf("status = %d, want 405", res.StatusCode)
	}
}
