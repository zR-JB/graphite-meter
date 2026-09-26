package endpoint

import (
	"encoding/json/v2"
	"net/http"
	"net/http/httptest"
	"testing"

	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

func TestProbeReturnsConnectionEvidenceAndLoad(t *testing.T) {
	rec := httptest.NewRecorder()
	NewProbe(nil, "", func() (int, int) { return 12, 256 }).ServeHTTP(rec, httptest.NewRequest(http.MethodGet, "http://meter/probe", nil))
	var got wire.Probe
	if err := json.Unmarshal(rec.Body.Bytes(), &got); err != nil {
		t.Fatal(err)
	}
	if got.ProtocolNegotiated != "http/1.1" || got.ClientIPVersion != 4 || got.ClientIPSource != "socket" {
		t.Fatalf("probe = %+v, want protocol http/1.1, IP version 4, source socket", got)
	}
	if got.Load == nil || got.Load.Active != 12 || got.Load.Max != 256 {
		t.Fatalf("probe load = %+v, want 12 of 256", got.Load)
	}
	if rec.Header().Get("Alt-Svc") != "" {
		t.Fatal("an ordinary probe advertised HTTP/3")
	}
}

func TestBootstrapProbeAdvertisesH3AndCloses(t *testing.T) {
	rec := httptest.NewRecorder()
	NewProbe(nil, "7249", nil).ServeHTTP(rec, httptest.NewRequest(http.MethodGet, "https://meter/probe", nil))
	if got, want := rec.Header().Get("Alt-Svc"), `h3=":7249"`; got != want {
		t.Fatalf("Alt-Svc = %q, want %q", got, want)
	}
	if got := rec.Header().Get("Connection"); got != "close" {
		t.Fatalf("Connection = %q, want %q", got, "close")
	}
}
