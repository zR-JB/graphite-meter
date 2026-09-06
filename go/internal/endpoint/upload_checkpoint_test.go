package endpoint

import (
	"encoding/json/v2"
	"net/http"
	"net/http/httptest"
	"testing"
	"time"
)

func TestUploadCheckpointRequiresAnExistingOwnedReceiverWithoutExtendingLifetime(t *testing.T) {
	store := NewUploadStore()
	id := store.Mint()
	checkpoint := httpAdapter(NewUploadCheckpoint(store, nil))
	request := func(owner string) *httptest.ResponseRecorder {
		r := httptest.NewRequest(http.MethodPost, "/upload/checkpoint?id="+id, nil)
		r.RemoteAddr = owner + ":1234"
		w := httptest.NewRecorder()
		checkpoint.ServeHTTP(w, r)
		return w
	}
	if w := request("192.0.2.1"); w.Code != 400 || store.live.Load() != 0 {
		t.Fatal("checkpoint allocated state for an unused id")
	}
	agg, access := store.getOrCreateFor(id, "192.0.2.1")
	if access != uploadAccessOK {
		t.Fatal(access)
	}
	agg.recordChunk(monoNanos()-int64(time.Second), 8192)
	touch := agg.lastTouchMono.Load()
	if w := request("192.0.2.2"); w.Code != 403 {
		t.Fatal("checkpoint exposed another owner's bytes")
	}
	var previous int64
	for range 2 {
		w := request("192.0.2.1")
		var snapshot struct {
			Bytes int64 `json:"bytes"`
			Nanos int64 `json:"nanos"`
		}
		if err := json.Unmarshal(w.Body.Bytes(), &snapshot); err != nil {
			t.Fatal(err)
		}
		if w.Code != 200 || snapshot.Bytes != 8192 || snapshot.Nanos <= previous {
			t.Fatalf("not a fresh zero-delivery observation: %d %+v", w.Code, snapshot)
		}
		previous = snapshot.Nanos
	}
	if agg.lastTouchMono.Load() != touch {
		t.Fatal("checkpoint extended upload activity lifetime")
	}
}

func TestDelegatedUploadOwnersShareTheParentRetentionBudget(t *testing.T) {
	store := NewUploadStore()
	for i := range maxLiveUploadsPerClient {
		owner := "principal:subject\x00browser-grant:first"
		if i%2 == 1 {
			owner = "principal:subject\x00browser-grant:second"
		}
		if _, access := store.getOrCreateFor(store.Mint(), owner); access != uploadAccessOK {
			t.Fatal(access)
		}
	}
	if _, access := store.getOrCreateFor(store.Mint(), "principal:subject\x00browser-grant:third"); access != uploadAccessClientFull {
		t.Fatal("another grant multiplied retention capacity")
	}
}
