package wire

import (
	"slices"
	"strings"
	"testing"
)

func TestCatalogConnectSourcesKeepIPv6InDiscoveryOnly(t *testing.T) {
	c := SingletonCatalog()
	c.Servers = append(c.Servers,
		ServerEntry{ID: "ipv6", Name: "IPv6", URL: "https://[2001:db8::1]", AdditionalOrigins: []string{"https://bulk.example:7249"}},
		ServerEntry{ID: "dns", Name: "DNS", URL: "https://meter.example", AdditionalOrigins: []string{"https://[2001:db8::2]:7248"}},
	)
	if err := c.Validate(); err != nil {
		t.Fatal(err)
	}
	sources := c.ConnectSources()
	if strings.Contains(strings.Join(sources, " "), "[") {
		t.Fatalf("IPv6 literal leaked into CSP sources: %v", sources)
	}
	for _, source := range []string{"https://meter.example:*", "wss://meter.example:*", "https://bulk.example:7249", "wss://bulk.example:7249"} {
		if !slices.Contains(sources, source) {
			t.Fatalf("missing configured DNS source %s: %v", source, sources)
		}
	}
	if !c.Servers[1].AllowsOrigin("https://[2001:db8::1]:7249") || !c.Servers[2].AllowsOrigin("https://[2001:db8::2]:7248") {
		t.Fatal("browser CSP filtering altered the native discovery boundary")
	}
}

func TestCatalogDiscoveryBoundary(t *testing.T) {
	s := ServerEntry{URL: "https://meter.example", AdditionalOrigins: []string{"https://transfer.example:7248"}}
	for raw, want := range map[string]bool{".": true, "https://meter.example:7249": true, "https://transfer.example:7248": true, "https://transfer.example:7247": false, "https://sub.meter.example": false, "http://meter.example": true, "https://user@meter.example": false, "https://meter.example/path": false} {
		if got := s.AllowsOrigin(raw); got != want {
			t.Errorf("AllowsOrigin(%q)=%v, want %v", raw, got, want)
		}
	}
}

func TestCatalogSelectionAndResolution(t *testing.T) {
	c := SingletonCatalog()
	c.Servers = append(c.Servers, ServerEntry{ID: "remote", URL: "https://remote.example", Name: "Remote"})
	if err := c.Validate(); err != nil {
		t.Fatal(err)
	}
	for _, ids := range [][]string{nil, {"self", "self"}, {"missing"}, {"self", "remote", "x", "y", "z"}} {
		if c.ValidateSelection(ids) == nil {
			t.Errorf("accepted %v", ids)
		}
	}
	resolved := c.Resolve("https://local.example:443")
	if resolved.Servers[0].URL != "https://local.example" || c.Servers[0].URL != "." {
		t.Fatalf("resolution mutated catalogue: %+v / %+v", c, resolved)
	}
}
