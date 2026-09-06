package server

import (
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"

	"github.com/zR-JB/graphite-meter/go/internal/config"
	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

func TestPublicConnectionPolicyKeepsSelfAndDNSSourcesForIPv6Page(t *testing.T) {
	cfg := config.Default()
	cfg.ServerCatalog = wire.SingletonCatalog()
	cfg.ServerCatalog.Servers = append(cfg.ServerCatalog.Servers, wire.ServerEntry{ID: "remote", Name: "Remote", URL: "https://meter.example", AdditionalOrigins: []string{"https://[2001:db8::2]:7248"}})
	handler := publicConnectionPolicy(&cfg, http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) { w.WriteHeader(http.StatusOK) }))
	r := httptest.NewRequest(http.MethodGet, "http://[::1]:7246/", nil)
	w := httptest.NewRecorder()
	handler.ServeHTTP(w, r)
	policy := w.Header().Get("Content-Security-Policy")
	if strings.Contains(policy, "[") || !strings.Contains(policy, "connect-src 'self' ") || !strings.Contains(policy, "https://meter.example:*") {
		t.Fatalf("unexpected public connection policy: %s", policy)
	}
}
