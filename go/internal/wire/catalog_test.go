package wire

import "testing"

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
