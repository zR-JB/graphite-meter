package endpoint

import (
	"encoding/json"
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
	httpAdapter(NewProbe(&cfg, "")).ServeHTTP(rec, req)
	var got wire.Probe
	if err := json.Unmarshal(rec.Body.Bytes(), &got); err != nil {
		t.Fatal(err)
	}
	if got.ProtocolNegotiated != "http/1.1" || got.ClientIPVersion != 4 || got.ClientIPSource != "socket" {
		t.Fatalf("probe = %+v", got)
	}
}

func TestBootstrapProbeAdvertisesH3AndCloses(t *testing.T) {
	cfg := config.Default()
	rec := httptest.NewRecorder()
	httpAdapter(NewProbe(&cfg, "7249")).ServeHTTP(rec, httptest.NewRequest(http.MethodGet, "https://meter/probe", nil))
	if got := rec.Header().Get("Alt-Svc"); got != `h3=":7249"` {
		t.Fatalf("Alt-Svc = %q", got)
	}
	if got := rec.Header().Get("Connection"); got != "close" {
		t.Fatalf("Connection = %q", got)
	}
}
