package endpoint

import (
	"encoding/json/v2"
	"fmt"
	"net/http"
	"net/url"
	"slices"
	"strings"

	"github.com/zR-JB/graphite-meter/go/internal/config"
	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

type ServerCatalog struct {
	cfg       *config.Config
	preflight *Preflight
}

func NewServerCatalog(cfg *config.Config) *ServerCatalog {
	return &ServerCatalog{cfg: cfg, preflight: NewPreflight(cfg)}
}

func (e *ServerCatalog) HandleHTTP(w http.ResponseWriter, r *http.Request) error {
	if r.Method != http.MethodGet {
		w.WriteHeader(http.StatusMethodNotAllowed)
		return nil
	}
	c := e.cfg.ServerCatalog
	if len(c.Servers) == 0 {
		c = wire.SingletonCatalog()
	}
	c.Servers = slices.Clone(c.Servers)
	c.Servers[0].Name, c.Servers[0].Location = e.cfg.ServerName, e.cfg.ServerLocation
	c.Servers[0].AdditionalOrigins = slices.Clone(c.Servers[0].AdditionalOrigins)
	for _, origin := range e.preflight.ConnectOrigins((&url.URL{Host: r.Host}).Hostname()) {
		if strings.HasPrefix(origin, "http://") || strings.HasPrefix(origin, "https://") {
			c.Servers[0].AdditionalOrigins = append(c.Servers[0].AdditionalOrigins, origin)
		}
	}
	if err := c.Validate(); err != nil {
		return fmt.Errorf("published catalogue: %w", err)
	}
	data, err := json.Marshal(c)
	if err != nil {
		return err
	}
	if len(data) > 64<<10 {
		return fmt.Errorf("published catalogue exceeds 64 KiB")
	}
	w.Header().Set("Content-Type", "application/json")
	w.Header().Set("Cache-Control", "no-store")
	_, err = w.Write(data)
	return err
}
