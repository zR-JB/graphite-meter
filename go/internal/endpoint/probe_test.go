package endpoint

import (
	"encoding/json/v2"
	"net/http"
	"net/http/httptest"
	"testing"

	"github.com/zR-JB/graphite-meter/go/internal/config"
	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

func TestProbeReturnsConnectionEvidence(t *testing.T) {
	cfg := config.Default()
	rec := httptest.NewRecorder()
	req := httptest.NewRequest(http.MethodGet, "http://meter/probe", nil)
	httpAdapter(NewProbe(&cfg, "", nil)).ServeHTTP(rec, req)
	var got wire.Probe
	if err := json.Unmarshal(rec.Body.Bytes(), &got); err != nil {
		t.Fatal(err)
	}
	if got.ProtocolNegotiated != "http/1.1" || got.ClientIPVersion != 4 || got.ClientIPSource != "socket" {
		t.Fatalf("probe = %+v, want protocol http/1.1, IP version 4, source socket", got)
	}
}

func TestProbeReportsServerLoad(t *testing.T) {
	cfg := config.Default()
	rec := httptest.NewRecorder()
	req := httptest.NewRequest(http.MethodGet, "http://meter/probe", nil)
	httpAdapter(NewProbe(&cfg, "", func() (int, int) { return 12, 256 })).ServeHTTP(rec, req)
	var got wire.Probe
	if err := json.Unmarshal(rec.Body.Bytes(), &got); err != nil {
		t.Fatal(err)
	}
	if got.Load == nil || got.Load.Active != 12 || got.Load.Max != 256 {
		t.Fatalf("probe load = %+v, want 12 of 256", got.Load)
	}
}

func TestBootstrapProbeAdvertisesH3AndCloses(t *testing.T) {
	cfg := config.Default()
	rec := httptest.NewRecorder()
	httpAdapter(NewProbe(&cfg, "7249", nil)).ServeHTTP(rec, httptest.NewRequest(http.MethodGet, "https://meter/probe", nil))
	if got, want := rec.Header().Get("Alt-Svc"), `h3=":7249"`; got != want {
		t.Fatalf("Alt-Svc = %q, want %q", got, want)
	}
	if got := rec.Header().Get("Connection"); got != "close" {
		t.Fatalf("Connection = %q, want %q", got, "close")
	}
}
