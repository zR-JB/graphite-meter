package endpoint

import (
	"encoding/json/v2"
	"net/http"
	"net/http/httptest"
	"net/netip"
	"reflect"
	"strings"
	"testing"

	"github.com/zR-JB/graphite-meter/go/internal/apipin"
	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

func TestProbeReturnsConnectionEvidence(t *testing.T) {
	for _, tc := range []struct {
		name, bootstrap, url, altSvc, connection string
		load                                     *wire.ProbeLoad
	}{
		{"ordinary", "", "http://meter/probe", "", "", &wire.ProbeLoad{Active: 12, Max: 256}},
		{"H3 bootstrap", "7249", "https://meter/probe", `h3=":7249"`, "close", nil},
	} {
		t.Run(tc.name, func(t *testing.T) {
			var load LoadFunc
			if tc.load != nil {
				load = func() (int, int) { return tc.load.Active, tc.load.Max }
			}
			rec := httptest.NewRecorder()
			NewProbe(nil, tc.bootstrap, load).ServeHTTP(rec, httptest.NewRequest(http.MethodGet, tc.url, nil))
			apipin.Validate(t, apipin.Schema(t, "probe"), rec.Body.Bytes())
			var got wire.Probe
			if err := json.Unmarshal(rec.Body.Bytes(), &got); err != nil {
				t.Fatal(err)
			}
			if got.ProtocolNegotiated != "http/1.1" || got.ClientIPVersion != 4 || got.ClientIPSource != "socket" ||
				!reflect.DeepEqual(got.Load, tc.load) {
				t.Fatalf("probe = %+v with load %+v, want http/1.1 over IPv4 from the socket with load %+v",
					got, got.Load, tc.load)
			}
			if rec.Header().Get("Alt-Svc") != tc.altSvc || rec.Header().Get("Connection") != tc.connection {
				t.Fatalf("headers = %v, want Alt-Svc %q and Connection %q", rec.Header(), tc.altSvc, tc.connection)
			}
		})
	}
}

// Evidence a trusted proxy leaves ambiguous names no client, as admission refuses it.
func TestProbeRefusesAmbiguousProxyEvidence(t *testing.T) {
	r := httptest.NewRequest(http.MethodGet, "http://meter/probe", nil)
	r.RemoteAddr = "10.0.0.2:1234"
	rec := httptest.NewRecorder()
	NewProbe([]netip.Prefix{netip.MustParsePrefix("10.0.0.0/8")}, "", nil).ServeHTTP(rec, r)
	if rec.Code != http.StatusBadRequest || strings.Contains(rec.Body.String(), "10.0.0.2") {
		t.Fatalf("probe = %d %q, want 400 naming no address", rec.Code, rec.Body.String())
	}
}
