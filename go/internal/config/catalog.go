package config

import (
	"encoding/json/v2"
	"fmt"
	"io"
	"os"

	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

const maxCatalogBytes = 64 << 10

func loadServerCatalog() (wire.ServerCatalog, error) {
	raw, inline := os.LookupEnv("GM_SERVER_CATALOG")
	path, file := os.LookupEnv("GM_SERVER_CATALOG_FILE")
	if inline && file {
		return wire.ServerCatalog{}, fmt.Errorf("set only one of GM_SERVER_CATALOG and GM_SERVER_CATALOG_FILE")
	}
	if !inline && !file {
		return wire.SingletonCatalog(), nil
	}
	data := []byte(raw)
	if file {
		f, err := os.Open(path)
		if err != nil {
			return wire.ServerCatalog{}, fmt.Errorf("GM_SERVER_CATALOG_FILE: %w", err)
		}
		defer f.Close()
		data, err = io.ReadAll(io.LimitReader(f, maxCatalogBytes+1))
		if err != nil {
			return wire.ServerCatalog{}, err
		}
	}
	if len(data) > maxCatalogBytes {
		return wire.ServerCatalog{}, fmt.Errorf("server catalogue exceeds 64 KiB")
	}
	var c wire.ServerCatalog
	if err := json.Unmarshal(data, &c, json.RejectUnknownMembers(true)); err != nil {
		return c, fmt.Errorf("server catalogue: %w", err)
	}
	for i := range c.Servers {
		if c.Servers[i].ID == "self" {
			return c, fmt.Errorf("self is added automatically; omit it from servers")
		}
		canonical, err := wire.CanonicalOrigin(c.Servers[i].URL)
		if err != nil {
			return c, fmt.Errorf("server %q: %w", c.Servers[i].ID, err)
		}
		c.Servers[i].URL = canonical
		for j, raw := range c.Servers[i].AdditionalOrigins {
			canonical, err := wire.CanonicalOrigin(raw)
			if err != nil {
				return c, err
			}
			c.Servers[i].AdditionalOrigins[j] = canonical
		}
	}
	c.Servers = append(wire.SingletonCatalog().Servers, c.Servers...)
	if c.DefaultSelection == nil {
		c.DefaultSelection = []string{"self"}
	}
	if data, err := json.Marshal(c); err != nil || len(data) > 48<<10 {
		return c, fmt.Errorf("normalized catalogue exceeds 48 KiB; reserve space for this server's transport origins")
	}
	return c, c.Validate()
}
