package wire

import (
	"fmt"
	"net"
	"net/url"
	"slices"
	"strings"

	"github.com/zR-JB/graphite-meter/go/internal/origin"
)

const MaxCatalogServers = 32
const MaxSelectedServers = 4

// ServerEntry identifies one measurement authority, independently of its transport ports.
type ServerEntry struct {
	ID                string   `json:"id"`
	URL               string   `json:"url"`
	Name              string   `json:"name"`
	Location          string   `json:"location,omitempty"`
	AdditionalOrigins []string `json:"additionalOrigins,omitempty"`
}

type ServerCatalog struct {
	DefaultSelection []string      `json:"defaultSelection"`
	Servers          []ServerEntry `json:"servers"`
}

func SingletonCatalog() ServerCatalog {
	return ServerCatalog{DefaultSelection: []string{"self"}, Servers: []ServerEntry{{ID: "self", URL: ".", Name: "graphite-meter"}}}
}

func (c ServerCatalog) Validate() error {
	if len(c.Servers) < 1 || len(c.Servers) > MaxCatalogServers || c.Servers[0].ID != "self" {
		return fmt.Errorf("catalogue requires self followed by at most %d additional servers", MaxCatalogServers-1)
	}
	ids, origins := map[string]bool{}, map[string]bool{}
	for _, entry := range c.Servers {
		if len(entry.ID) == 0 || len(entry.ID) > 64 || strings.ContainsFunc(entry.ID, func(r rune) bool {
			return !(r >= 'a' && r <= 'z' || r >= 'A' && r <= 'Z' || r >= '0' && r <= '9' || r == '.' || r == '_' || r == '-')
		}) || len(entry.Name) > 256 || len(entry.Location) > 256 || strings.ContainsFunc(entry.Name+entry.Location, func(r rune) bool { return r < 32 || r == 127 }) {
			return fmt.Errorf("invalid catalogue server identity")
		}
		if ids[entry.ID] || origins[origin.Key(entry.URL)] {
			return fmt.Errorf("duplicate catalogue server %q", entry.ID)
		}
		ids[entry.ID], origins[origin.Key(entry.URL)] = true, true
		if _, err := CanonicalOrigin(entry.URL); (err != nil && entry.URL != ".") || entry.URL == "." && entry.ID != "self" {
			return fmt.Errorf("invalid catalogue origin for %q", entry.ID)
		}
		if len(entry.AdditionalOrigins) > 32 {
			return fmt.Errorf("too many additional origins for %q", entry.ID)
		}
		for _, raw := range entry.AdditionalOrigins {
			if _, err := CanonicalOrigin(raw); err != nil {
				return fmt.Errorf("invalid additional origin for %q", entry.ID)
			}
		}
	}
	return c.ValidateSelection(c.DefaultSelection)
}

func (c ServerCatalog) ValidateSelection(selected []string) error {
	if len(selected) < 1 || len(selected) > MaxSelectedServers {
		return fmt.Errorf("select one to %d servers", MaxSelectedServers)
	}
	seen := map[string]bool{}
	for _, id := range selected {
		if seen[id] || !slices.ContainsFunc(c.Servers, func(s ServerEntry) bool { return s.ID == id }) {
			return fmt.Errorf("unknown or repeated server %q", id)
		}
		seen[id] = true
	}
	return nil
}

func (c ServerCatalog) Resolve(base string) ServerCatalog {
	c.Servers = slices.Clone(c.Servers)
	for i := range c.Servers {
		if c.Servers[i].URL == "." {
			c.Servers[i].URL = origin.Key(base)
		} else {
			c.Servers[i].URL = origin.Key(c.Servers[i].URL)
		}
	}
	return c
}

// AllowsOrigin constrains discovery; it never grants credential access.
func (s ServerEntry) AllowsOrigin(raw string) bool {
	if raw == "." {
		return true
	}
	u, err := targetOrigin(raw)
	if err != nil {
		return false
	}
	base, err := url.Parse(s.URL)
	if err != nil {
		return false
	}
	if strings.EqualFold(u.Hostname(), base.Hostname()) {
		return true
	}
	return slices.ContainsFunc(s.AdditionalOrigins, func(allowed string) bool { return origin.Equal(raw, allowed) })
}

func (s ServerEntry) ValidateDiscovery(p Preflight) error {
	if err := p.Validate(); err != nil {
		return err
	}
	for _, t := range p.Capabilities.ThroughputTargets {
		if !s.AllowsOrigin(t.Origin) {
			return fmt.Errorf("server %q advertised an unapproved throughput origin", s.ID)
		}
	}
	for _, t := range p.Capabilities.LatencyTargets {
		if !s.AllowsOrigin(t.Origin) {
			return fmt.Errorf("server %q advertised an unapproved latency origin", s.ID)
		}
	}
	return nil
}

// ConnectSources admits the configured hostname's transport ports, never subdomains.
func (c ServerCatalog) ConnectSources() []string {
	var out []string
	for _, s := range c.Servers {
		if s.URL == "." {
			continue
		}
		u, err := url.Parse(s.URL)
		if err != nil {
			continue
		}
		host := u.Hostname()
		if strings.Contains(host, ":") {
			host = "[" + host + "]"
		}
		out = append(out, "http://"+host+":*", "https://"+host+":*", "ws://"+host+":*", "wss://"+host+":*")
		for _, raw := range s.AdditionalOrigins {
			out = append(out, raw, strings.Replace(raw, "http", "ws", 1))
		}
	}
	return out
}

// CanonicalOrigin is shared by catalogue decoders and authentication audiences.
func CanonicalOrigin(raw string) (string, error) {
	u, err := targetOrigin(raw)
	if err != nil || raw == "." {
		return "", fmt.Errorf("expected an absolute HTTP(S) origin")
	}
	if u.Port() == "0" || strings.ContainsAny(u.Hostname(), "*;") {
		return "", fmt.Errorf("invalid origin host or port")
	}
	if strings.Contains(u.Hostname(), ":") && net.ParseIP(u.Hostname()) == nil {
		return "", fmt.Errorf("invalid IPv6 origin")
	}
	return origin.Key(raw), nil
}
