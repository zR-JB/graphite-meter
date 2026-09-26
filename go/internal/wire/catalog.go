package wire

import (
	"fmt"
	"net"
	"net/url"
	"slices"
	"strings"
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
	return ServerCatalog{DefaultSelection: []string{"self"},
		Servers: []ServerEntry{{ID: "self", URL: ".", Name: "graphite-meter"}}}
}

func (c ServerCatalog) Validate() error {
	if len(c.Servers) < 1 || len(c.Servers) > MaxCatalogServers || c.Servers[0].ID != "self" {
		return fmt.Errorf("catalogue requires self followed by at most %d additional servers", MaxCatalogServers-1)
	}
	ids, origins := map[string]bool{}, map[string]bool{}
	for _, entry := range c.Servers {
		if len(entry.ID) == 0 || len(entry.ID) > 64 || strings.ContainsFunc(entry.ID, func(r rune) bool {
			return !(r >= 'a' && r <= 'z' || r >= 'A' && r <= 'Z' || r >= '0' && r <= '9' ||
				r == '.' || r == '_' || r == '-')
		}) || len(entry.Name) > 256 || len(entry.Location) > 256 || !SafeText(entry.Name+entry.Location) {
			return fmt.Errorf("invalid catalogue server identity")
		}
		key, err := CanonicalOrigin(entry.URL)
		if entry.URL == "." && entry.ID != "self" || entry.URL != "." && err != nil {
			return fmt.Errorf("invalid catalogue origin for %q", entry.ID)
		}
		if ids[entry.ID] || origins[key] {
			return fmt.Errorf("duplicate catalogue server %q", entry.ID)
		}
		ids[entry.ID], origins[key] = true, true
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
	for i, s := range c.Servers {
		raw := s.URL
		if raw == "." {
			raw = base
		}
		c.Servers[i].URL, _ = OriginKey(raw)
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
	return slices.ContainsFunc(s.AdditionalOrigins, func(allowed string) bool { return SameOrigin(raw, allowed) })
}

func (s ServerEntry) ValidateDiscovery(p Preflight) error {
	if err := p.Validate(); err != nil {
		return err
	}
	var origins []string
	for _, t := range p.Capabilities.ThroughputTargets {
		origins = append(origins, t.Origin)
	}
	for _, t := range p.Capabilities.LatencyTargets {
		origins = append(origins, t.Origin)
	}
	if !slices.ContainsFunc(origins, func(o string) bool { return !s.AllowsOrigin(o) }) {
		return nil
	}
	return fmt.Errorf("server %q advertised an unapproved target origin", s.ID)
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
		if !strings.Contains(host, ":") {
			out = append(out, "http://"+host+":*", "https://"+host+":*", "ws://"+host+":*", "wss://"+host+":*")
		}
		for _, raw := range s.AdditionalOrigins {
			if BrowserConnectSourceSupported(raw) {
				out = append(out, raw, strings.Replace(raw, "http", "ws", 1))
			}
		}
	}
	return out
}

// BrowserConnectSourceSupported excludes IPv6 literals, which CSP host sources cannot express.
func BrowserConnectSourceSupported(raw string) bool {
	return !strings.Contains(raw, "://[")
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
	key, _ := OriginKey(raw)
	return key, nil
}

// OriginKey is raw's scheme and host, lowercased and without a default port; ok is false if raw has neither.
func OriginKey(raw string) (string, bool) {
	u, err := url.Parse(raw)
	if err != nil || u.Scheme == "" || u.Hostname() == "" {
		return "", false
	}
	scheme, host, port := strings.ToLower(u.Scheme), strings.ToLower(u.Hostname()), u.Port()
	if scheme == "http" && port == "80" || scheme == "https" && port == "443" {
		port = ""
	}
	if port != "" {
		return scheme + "://" + net.JoinHostPort(host, port), true
	}
	if strings.Contains(host, ":") {
		// Hostname strips the brackets an IPv6 literal needs to be a valid authority.
		return scheme + "://[" + host + "]", true
	}
	return scheme + "://" + host, true
}

// SameOrigin reports whether a and b name one origin; text that is no origin matches nothing.
func SameOrigin(a, b string) bool {
	keyA, okA := OriginKey(a)
	keyB, okB := OriginKey(b)
	return okA && okB && keyA == keyB
}
