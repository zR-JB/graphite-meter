package config

import (
	"os"
	"testing"
)

func TestDefault(t *testing.T) {
	c := Default()
	if c.H1Addr != ":8765" || c.H2Addr != ":8443" || c.H3Addr != ":8444" {
		t.Fatalf("addresses = %q %q %q", c.H1Addr, c.H2Addr, c.H3Addr)
	}
	if c.EnableH2 || c.EnableH3 {
		t.Fatal("TLS protocols enabled by default")
	}
}

func TestLoadProtocolEnvironment(t *testing.T) {
	t.Setenv("GM_ENABLE_H2", "true")
	t.Setenv("GM_ENABLE_H3", "1")
	t.Setenv("GM_TLS_CERT", "/cert.pem")
	t.Setenv("GM_TLS_KEY", "/key.pem")
	t.Setenv("PUBLIC_H1_ORIGIN", "http://meter.example:8765")
	t.Setenv("PUBLIC_H2_ORIGIN", "https://meter.example:8443")
	t.Setenv("PUBLIC_H3_ORIGIN", "https://meter.example:8444")
	c, err := Load()
	if err != nil {
		t.Fatal(err)
	}
	if !c.EnableH2 || !c.EnableH3 || c.PublicH2Origin != "https://meter.example:8443" {
		t.Fatalf("config = %+v", c)
	}
}

func TestLoadRejectsInvalidBool(t *testing.T) {
	t.Setenv("GM_ENABLE_H2", "sometimes")
	if _, err := Load(); err == nil {
		t.Fatal("invalid bool accepted")
	}
}

func TestValidateRequiresTLSFiles(t *testing.T) {
	c := Default()
	c.EnableH2 = true
	if err := c.Validate(); err == nil {
		t.Fatal("enabled TLS without certificate accepted")
	}
	c.TLSCert, c.TLSKey = "/cert", "/key"
	if err := c.Validate(); err != nil {
		t.Fatal(err)
	}
}

func TestValidateOriginsAndAddresses(t *testing.T) {
	c := Default()
	c.PublicH2Origin = "http://meter.example"
	if err := c.Validate(); err == nil {
		t.Fatal("clear h2 origin accepted")
	}
	c = Default()
	c.EnableH2, c.EnableH3 = true, true
	c.TLSCert, c.TLSKey = "c", "k"
	c.H3Addr = c.H2Addr
	if err := c.Validate(); err == nil {
		t.Fatal("colliding TCP listeners accepted")
	}
}

func TestLoadTrustedProxies(t *testing.T) {
	t.Setenv("GM_TRUSTED_PROXIES", "10.0.0.0/8, 2001:db8::/32")
	c, err := Load()
	if err != nil {
		t.Fatal(err)
	}
	if len(c.TrustedProxies) != 2 {
		t.Fatalf("trusted proxies = %d", len(c.TrustedProxies))
	}
}

func TestLoadUsesDefaultsWithCleanEnvironment(t *testing.T) {
	for _, k := range []string{"GM_ENABLE_H2", "GM_ENABLE_H3", "GM_TLS_CERT", "GM_TLS_KEY", "PUBLIC_H1_ORIGIN", "PUBLIC_H2_ORIGIN", "PUBLIC_H3_ORIGIN", "GM_TRUSTED_PROXIES"} {
		old, ok := os.LookupEnv(k)
		os.Unsetenv(k)
		if ok {
			defer os.Setenv(k, old)
		}
	}
	c, err := Load()
	if err != nil {
		t.Fatal(err)
	}
	if c.H1Addr != ":8765" || c.H2Addr != ":8443" || c.H3Addr != ":8444" || c.EnableH2 || c.EnableH3 || len(c.TrustedProxies) != 0 {
		t.Fatalf("load defaults = %+v", c)
	}
}
