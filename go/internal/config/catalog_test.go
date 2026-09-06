package config

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func TestServerCatalogConfiguration(t *testing.T) {
	for _, file := range []bool{false, true} {
		t.Run(map[bool]string{false: "inline", true: "file"}[file], func(t *testing.T) {
			unsetEnv(t, "GM_SERVER_CATALOG")
			unsetEnv(t, "GM_SERVER_CATALOG_FILE")
			raw := `{"defaultSelection":["remote"],"servers":[{"id":"remote","name":"Remote","url":"https://EXAMPLE.net:443","additionalOrigins":["https://transfer.example.net"]}]}`
			if file {
				path := filepath.Join(t.TempDir(), "servers.json")
				if err := os.WriteFile(path, []byte(raw), 0600); err != nil {
					t.Fatal(err)
				}
				t.Setenv("GM_SERVER_CATALOG_FILE", path)
			} else {
				t.Setenv("GM_SERVER_CATALOG", raw)
			}
			c, err := Load()
			if err != nil {
				t.Fatal(err)
			}
			if len(c.ServerCatalog.Servers) != 2 || c.ServerCatalog.Servers[0].ID != "self" || c.ServerCatalog.DefaultSelection[0] != "remote" || c.ServerCatalog.Servers[1].URL != "https://example.net" {
				t.Fatalf("catalogue = %+v", c.ServerCatalog)
			}
		})
	}
}

func TestServerCatalogRejectsUnsafeOrAmbiguousInput(t *testing.T) {
	unsetEnv(t, "GM_SERVER_CATALOG_FILE")
	for _, raw := range []string{
		`{"defaultSelection":[],"servers":[]}`,
		`{"defaultSelection":["missing"],"servers":[]}`,
		`{"servers":[{"id":"self","url":"https://example.net"}]}`,
		`{"servers":[{"id":"a","url":"https://example.net"},{"id":"b","url":"https://EXAMPLE.net:443"}]}`,
		`{"servers":[{"id":"a","url":"https://user:pass@example.net"}]}`,
		`{"servers":[{"id":"a","url":"https://example.net/path"}]}`,
		`{"servers":[{"id":"a","url":"https://*.example.net"}]}`,
		`{"servers":[{"id":"a","url":"https://example.net;evil"}]}`,
		`{"servers":[],"typo":true}`,
		strings.Repeat(" ", 64*1024+1),
	} {
		t.Run(raw[:min(60, len(raw))], func(t *testing.T) {
			t.Setenv("GM_SERVER_CATALOG", raw)
			if _, err := Load(); err == nil {
				t.Fatal("invalid catalogue accepted")
			}
		})
	}
	t.Setenv("GM_SERVER_CATALOG", `{}`)
	t.Setenv("GM_SERVER_CATALOG_FILE", "/unused")
	if _, err := Load(); err == nil {
		t.Fatal("ambiguous sources accepted")
	}
}

func TestOriginCatalogueStableIdentityAndSources(t *testing.T) {
	unsetEnv(t, "GM_SERVER_CATALOG_FILE")
	t.Setenv("GM_SERVER_CATALOG", `["https://EXAMPLE.net:443", "http://[::1]:8080"]`)
	first, err := loadServerCatalog()
	if err != nil {
		t.Fatal(err)
	}
	if len(first.Servers) != 3 || first.Servers[1].URL != "https://example.net" || first.Servers[1].Name != "example.net" || first.Servers[2].Name != "[::1]:8080" || first.DefaultSelection[0] != "self" {
		t.Fatalf("catalogue: %+v", first)
	}
	t.Setenv("GM_SERVER_CATALOG", `["http://[::1]:8080", "https://example.net/"]`)
	second, err := loadServerCatalog()
	if err != nil {
		t.Fatal(err)
	}
	if first.Servers[1].ID != second.Servers[2].ID || first.Servers[2].ID != second.Servers[1].ID {
		t.Fatal("reordering changed identities")
	}
	path := filepath.Join(t.TempDir(), "servers.json")
	if err := os.WriteFile(path, []byte(`["https://example.net"]`), 0600); err != nil {
		t.Fatal(err)
	}
	unsetEnv(t, "GM_SERVER_CATALOG")
	t.Setenv("GM_SERVER_CATALOG_FILE", path)
	file, err := loadServerCatalog()
	if err != nil || file.Servers[1].ID != first.Servers[1].ID {
		t.Fatalf("file catalogue: %+v %v", file, err)
	}
}

func TestOriginCatalogueRejectsInvalidEntries(t *testing.T) {
	unsetEnv(t, "GM_SERVER_CATALOG_FILE")
	for _, raw := range []string{
		`["https://example.net", "https://EXAMPLE.net:443"]`, `["https://example.net/probe"]`, `["https://user:pass@example.net"]`, `["https://example.net?x=1"]`, `["https://example.net#x"]`, `["https://*.example.net"]`, `[null]`, `[42]`, `[{}]`, `[`,
	} {
		t.Run(raw, func(t *testing.T) {
			t.Setenv("GM_SERVER_CATALOG", raw)
			if _, err := loadServerCatalog(); err == nil {
				t.Fatal("invalid origin list accepted")
			}
		})
	}
}
