package endpoint

import (
	"bytes"
	"context"
	"encoding/json/v2"
	"errors"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
	"testing/synctest"
	"time"

	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

func TestUploadSessionMintsFreshIDsWithoutState(t *testing.T) {
	store := NewUpload(nil, nil)
	mint := func() string {
		rec := httptest.NewRecorder()
		store.ServeSession(rec, httptest.NewRequest(http.MethodPost, "/upload/session", nil))
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
			w := httptest.NewRecorder()
			store.ServeCheckpoint(w, r)
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

func TestUploadCountsEchoesAndAggregates(t *testing.T) {
	store := NewUpload(nil, nil)
	srv := httptest.NewServer(store)
	defer srv.Close()
	for _, n := range []int64{3*1024*1024 + 123, 0} {
		id := store.Mint()
		res, err := http.Post(srv.URL+"/upload?id="+id, "application/octet-stream", bytes.NewReader(make([]byte, n)))
		if err != nil {
			t.Fatalf("post: %v", err)
		}
		var echo struct {
			Bytes int64 `json:"bytes"`
		}
		err = json.UnmarshalRead(res.Body, &echo)
		res.Body.Close()
		if err != nil || echo.Bytes != n || res.Header.Get("Content-Type") != "application/json" ||
			res.Header.Get("Cache-Control") != "no-store" {
			t.Fatalf("echo = %d %v with headers %v, want %d bytes", echo.Bytes, err, res.Header, n)
		}
		if agg, ok := store.get(id); !ok || agg.bytes.Load() != n || store.lanesOf(agg) != 0 {
			t.Fatalf("aggregate for %d bytes = %v, want the count with no lane left", n, agg)
		}
	}
}

// The echo is receiver evidence: it reports the bytes read, never the length the request declared.
func TestUploadEchoReportsReceivedBytesNotTheDeclaredLength(t *testing.T) {
	store := NewUpload(nil, nil)
	r := httptest.NewRequest(http.MethodPost, "/upload?id="+store.Mint(), strings.NewReader("12345"))
	r.ContentLength = 1 << 30
	w := httptest.NewRecorder()
	store.ServeHTTP(w, r)
	var echo struct {
		Bytes int64 `json:"bytes"`
	}
	if err := json.Unmarshal(w.Body.Bytes(), &echo); err != nil || w.Code != http.StatusOK || echo.Bytes != 5 {
		t.Fatalf("echo = %d %s (%v), want the 5 received bytes", w.Code, w.Body.String(), err)
	}
}

func TestUploadHTTPRequiresAnOwnerBoundIDBeforeReading(t *testing.T) {
	for _, tc := range []struct {
		id   string
		want int
	}{{"", 400}, {"forged", 400}, {"another-owner", 403}, {"over-cap", 503}} {
		t.Run(tc.id, func(t *testing.T) {
			store := NewUpload(nil, nil)
			id := tc.id
			switch id {
			case "another-owner":
				id = store.Mint()
				store.getOrCreateFor(id, "different-owner")
			case "over-cap":
				fillStore(store)
				id = store.Mint()
			}
			live := store.live()
			body := strings.NewReader("must not be drained")
			rec := httptest.NewRecorder()
			store.ServeHTTP(rec, httptest.NewRequest(http.MethodPost, "/upload?id="+id, body))
			if rec.Code != tc.want || body.Len() != len("must not be drained") || store.live() != live {
				t.Fatalf("refusal = %d, unread = %d, live %d -> %d", rec.Code, body.Len(), live, store.live())
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
	store.ServeHTTP(rec,
		httptest.NewRequest(http.MethodPost, "/upload?id="+id, &errReader{remaining: 4096}))
	if rec.Body.Len() != 0 {
		t.Fatalf("aborted upload published response %q", rec.Body.String())
	}
	if agg, ok := store.get(id); !ok || agg.bytes.Load() != 4096 || store.lanesOf(agg) != 0 {
		t.Fatal("aborted HTTP upload lost its partial receiver count or retained its lane")
	}
}

// A stream carries no status line, so a refused lane is reported through Receive's error, before any read.
func TestUploadStreamRefusalsLeaveTheReceiverUnchanged(t *testing.T) {
	for _, tc := range []struct {
		name  string
		owner string
		setup func(*Upload, string)
		want  uploadAccess
	}{
		{"another client's lane", "other-owner", func(*Upload, string) {}, uploadAccessOwnerMismatch},
		{"after finish", "owner", func(s *Upload, id string) { s.finishFor(id, "owner") }, uploadAccessInvalid},
		{"over the global cap", "owner", func(s *Upload, _ string) { fillStore(s) }, uploadAccessGlobalFull},
	} {
		t.Run(tc.name, func(t *testing.T) {
			store := NewUpload(nil, nil)
			upload := store
			id := store.Mint()
			if tc.want != uploadAccessGlobalFull {
				n, err := upload.Receive(id, ownedBy("owner"), strings.NewReader("first"), &idleDeadline{})
				if err != nil || n != 5 {
					t.Fatalf("initial upload = %d, %v", n, err)
				}
			}
			tc.setup(store, id)
			body := strings.NewReader("must not be drained")
			n, err := upload.Receive(id, ownedBy(tc.owner), body, &idleDeadline{})
			refusal, ok := errors.AsType[*uploadRefusalError](err)
			if !ok || refusal.access != tc.want || n != 0 || body.Len() != len("must not be drained") ||
				!strings.Contains(err.Error(), uploadAccessInfos[tc.want].message) {
				t.Fatalf("refused lane = %d, %v; unread bytes = %d", n, err, body.Len())
			}
			if agg, ok := store.get(id); ok && (agg.bytes.Load() != 5 || store.lanesOf(agg) != 0) {
				t.Fatalf("refusal changed the receiver: bytes=%d posts=%d", agg.bytes.Load(), store.lanesOf(agg))
			}
		})
	}
}

type deadlineRecorder struct {
	http.ResponseWriter
	read time.Time
}

func (d *deadlineRecorder) SetReadDeadline(t time.Time) error {
	d.read = t
	return nil
}

// A stuck body read is bounded by the idle bound, or by the request's own earlier deadline.
func TestUploadBoundsItsBodyRead(t *testing.T) {
	for _, remaining := range []time.Duration{0, time.Second, time.Hour} {
		t.Run(remaining.String(), func(t *testing.T) {
			ctx := t.Context()
			want := time.Now().Add(remaining)
			if remaining != 0 {
				var cancel context.CancelFunc
				ctx, cancel = context.WithDeadline(ctx, want)
				defer cancel()
			}
			rec := &deadlineRecorder{ResponseWriter: httptest.NewRecorder()}
			store := NewUpload(nil, nil)
			before := time.Now()
			store.ServeHTTP(rec, httptest.NewRequestWithContext(ctx, http.MethodPost,
				"/upload?id="+store.Mint(), bytes.NewReader(make([]byte, 4096))))
			if remaining != 0 && remaining < wire.WTIdleBound {
				if !rec.read.Equal(want) {
					t.Fatalf("read deadline = %v, want the request deadline %v", rec.read, want)
				}
			} else if rec.read.Before(before.Add(wire.WTIdleBound)) ||
				rec.read.After(time.Now().Add(wire.WTIdleBound)) {
				t.Fatalf("read deadline %v does not keep the idle bound", rec.read)
			}
		})
	}
}
