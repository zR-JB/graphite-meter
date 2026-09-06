package server

import (
	"net/http"
	"net/url"
	"strings"

	"github.com/zR-JB/graphite-meter/go/internal/config"
	"github.com/zR-JB/graphite-meter/go/internal/endpoint"
	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

// Public pages use the same configured destination boundary as authenticated pages.
func publicConnectionPolicy(cfg *config.Config, next http.Handler) http.Handler {
	preflight := endpoint.NewPreflight(cfg)
	configured := cfg.ServerCatalog.ConnectSources()
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		sources := append([]string{"'self'"}, configured...)
		for _, raw := range preflight.ConnectOrigins((&url.URL{Host: r.Host}).Hostname()) {
			parsed := strings.Replace(strings.Replace(raw, "wss://", "https://", 1), "ws://", "http://", 1)
			if _, err := wire.CanonicalOrigin(parsed); err == nil && wire.BrowserConnectSourceSupported(raw) {
				sources = append(sources, raw)
			}
		}
		w.Header().Set("Content-Security-Policy", "connect-src "+strings.Join(sources, " "))
		next.ServeHTTP(w, r)
	})
}
