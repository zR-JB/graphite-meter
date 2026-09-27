package endpoint

import (
	"encoding/json/v2"
	"errors"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
	"testing/synctest"
	"time"

	"github.com/zR-JB/graphite-meter/go/internal/testkit"
	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

func TestUploadSessionMintsFreshIDsWithoutState(t *testing.T) {
	store := NewUpload(nil, nil)
	mint := func() string {
		rec := testkit.Record(store.ServeSession, httptest.NewRequest(http.MethodPost, "/upload/session", nil))
		var body struct {
			UploadID string `json:"uploadId"`
		}
		if err := json.Unmarshal(rec.Body.Bytes(), &body); err != nil || rec.Code != http.StatusOK {
			t.Fatalf("mint = %d %q: %v", rec.Code, rec.Body.String(), err)
		}
		return body.UploadID
	}
	a, b := mint(), mint()
	if a == b || !store.validID(a) || !store.validID(b) || store.live() != 0 {
		t.Fatalf("minted %q and %q with %d live receivers, want two distinct authenticated ids and no state",
			a, b, store.live())
	}
}

// A checkpoint observes an existing, owned receiver without allocating state or extending its lifetime.
func TestUploadCheckpointObservesWithoutExtendingLifetime(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		store := NewUpload(nil, nil)
		id := store.Mint()
		checkpoint := func(owner string) *httptest.ResponseRecorder {
			r := httptest.NewRequest(http.MethodPost, "/upload/checkpoint?id="+id, nil)
			r.RemoteAddr = owner + ":1234"
			w := testkit.Record(store.ServeCheckpoint, r)
			return w
		}
		if w := checkpoint("192.0.2.1"); w.Code != http.StatusBadRequest || store.live() != 0 {
			t.Fatal("checkpoint allocated state for an unused id")
		}
		agg, _ := store.getOrCreateFor(id, "192.0.2.1")
		agg.recordChunk(store.now(), 8192)
		touched := func() int64 {
			store.mu.Lock()
			defer store.mu.Unlock()
			return agg.lastTouch
		}
		touch := touched()
		if w := checkpoint("192.0.2.2"); w.Code != http.StatusForbidden {
			t.Fatal("checkpoint exposed another owner's bytes")
		}
		for step := range int64(2) {
			time.Sleep(time.Second)
			var snapshot struct {
				Bytes int64 `json:"bytes"`
				Nanos int64 `json:"nanos"`
			}
			w := checkpoint("192.0.2.1")
			err := json.Unmarshal(w.Body.Bytes(), &snapshot)
			if err != nil || w.Code != http.StatusOK || snapshot.Bytes != 8192 ||
				snapshot.Nanos != (step+1)*int64(time.Second) {
				t.Fatalf("checkpoint %d = %d %+v, want 8192 bytes over %ds of receiver time", step, w.Code, snapshot,
					step+1)
			}
		}
		if touched() != touch {
			t.Fatal("checkpoint extended the receiver's lifetime")
		}
	})
}

// The echo is receiver evidence: it reports the bytes read, never the length the request declared.
func TestUploadEchoReportsReceivedBytesNotTheDeclaredLength(t *testing.T) {
	store := NewUpload(nil, nil)
	r := httptest.NewRequest(http.MethodPost, "/upload?id="+store.Mint(), strings.NewReader("12345"))
	r.ContentLength = 1 << 30
	w := testkit.Record(store.Handler(wire.IdleBound).ServeHTTP, r)
	var echo struct {
		Bytes int64 `json:"bytes"`
	}
	if err := json.Unmarshal(w.Body.Bytes(), &echo); err != nil || w.Code != http.StatusOK || echo.Bytes != 5 ||
		w.Header().Get("Content-Type") != "application/json" || w.Header().Get("Cache-Control") != "no-store" {
		t.Fatalf("echo = %d %s (%v) with headers %v, want the 5 received bytes", w.Code, w.Body.String(), err,
			w.Header())
	}
}

// A refusal is classified before any read and leaves the receiver as it was; streams share it through Receive.
func TestUploadHTTPRequiresAnOwnerBoundIDBeforeReading(t *testing.T) {
	const owner = "192.0.2.1"
	seed := func(s *Upload, by string) string {
		id := s.Mint()
		if n, err := s.Receive(id, ownedBy(by), strings.NewReader("first"), &idleDeadline{}); err != nil || n != 5 {
			t.Fatalf("initial upload = %d, %v", n, err)
		}
		return id
	}
	for _, tc := range []struct {
		name   string
		setup  func(*Upload) string
		status int
		access uploadAccess
	}{
		{"missing id", func(*Upload) string { return "" }, 400, uploadAccessInvalid},
		{"forged id", func(*Upload) string { return "forged" }, 400, uploadAccessInvalid},
		{"another client's lane", func(s *Upload) string { return seed(s, "198.51.100.9") }, 403,
			uploadAccessOwnerMismatch},
		{"after finish", func(s *Upload) string {
			id := seed(s, owner)
			s.finishFor(id, owner)
			return id
		}, 400, uploadAccessInvalid},
		{"over the global cap", func(s *Upload) string {
			fillStore(s)
			return s.Mint()
		}, 503, uploadAccessGlobalFull},
	} {
		t.Run(tc.name, func(t *testing.T) {
			store := NewUpload(nil, nil)
			id := tc.setup(store)
			live := store.live()
			body := strings.NewReader("must not be drained")
			rec := testkit.Record(store.Handler(wire.IdleBound).ServeHTTP, httptest.NewRequest(http.MethodPost, "/upload?id="+id, body))
			if rec.Code != tc.status || !strings.Contains(rec.Body.String(), uploadAccessInfos[tc.access].message) ||
				body.Len() != len("must not be drained") || store.live() != live {
				t.Fatalf("refusal = %d %q, unread = %d, live %d -> %d", rec.Code, rec.Body.String(), body.Len(), live,
					store.live())
			}
			if agg, ok := store.get(id); ok && (agg.bytes.Load() != 5 || store.lanesOf(agg) != 0) {
				t.Fatalf("refusal changed the receiver: bytes=%d lanes=%d", agg.bytes.Load(), store.lanesOf(agg))
			}
		})
	}
}

// errReader yields remaining zero bytes then a non-EOF error, like a connection dropped mid-upload.
type errReader struct{ remaining int }

func (r *errReader) Read(p []byte) (int, error) {
	if r.remaining <= 0 {
		return 0, errors.New("simulated connection reset")
	}
	n := min(len(p), r.remaining)
	r.remaining -= n
	return n, nil
}

func TestUploadHTTPAbortKeepsThePartialCountWithoutPublishingIt(t *testing.T) {
	store := NewUpload(nil, nil)
	id := store.Mint()
	rec := httptest.NewRecorder()
	store.Handler(wire.IdleBound).ServeHTTP(rec,
		httptest.NewRequest(http.MethodPost, "/upload?id="+id, &errReader{remaining: 4096}))
	if rec.Body.Len() != 0 {
		t.Fatalf("aborted upload published response %q", rec.Body.String())
	}
	if agg, ok := store.get(id); !ok || agg.bytes.Load() != 4096 || store.lanesOf(agg) != 0 {
		t.Fatal("aborted HTTP upload lost its partial receiver count or retained its lane")
	}
}
