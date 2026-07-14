package config

import (
	"os"
	"testing"
)

func TestDefault(t *testing.T) {
	c := Default()
	if c.H1Addr != ":7246" || c.H1TLSAddr != ":7247" || c.H2Addr != ":7248" || c.H3Addr != ":7249" {
		t.Fatalf("addresses = %q %q %q %q", c.H1Addr, c.H1TLSAddr, c.H2Addr, c.H3Addr)
	}
	if c.EnableH1TLS || c.EnableH2 || c.EnableH3 {
		t.Fatal("TLS protocols enabled by default")
	}
}

func TestLoadProtocolEnvironment(t *testing.T) {
	t.Setenv("GM_ENABLE_H1_TLS", "true")
	t.Setenv("GM_ENABLE_H2", "true")
	t.Setenv("GM_ENABLE_H3", "1")
	t.Setenv("GM_TLS_CERT", "/cert.pem")
	t.Setenv("GM_TLS_KEY", "/key.pem")
	t.Setenv("PUBLIC_H1_ORIGIN", "http://meter.example:7246")
	t.Setenv("PUBLIC_H1_TLS_ORIGIN", "https://meter.example:7247")
	t.Setenv("PUBLIC_H2_ORIGIN", "https://meter.example:7248")
	t.Setenv("PUBLIC_H3_ORIGIN", "https://meter.example:7249")
	c, err := Load()
	if err != nil {
		t.Fatal(err)
	}
	if !c.EnableH1TLS || !c.EnableH2 || !c.EnableH3 || c.PublicH1TLSOrigin != "https://meter.example:7247" {
		t.Fatalf("config = %+v", c)
	}
}

func TestLoadLegacyTLSOrigin(t *testing.T) {
	t.Setenv("PUBLIC_TLS_ORIGIN", "https://legacy.example")
	c, err := Load()
	if err != nil {
		t.Fatal(err)
	}
	if c.PublicH2Origin != "https://legacy.example" {
		t.Fatalf("PublicH2Origin = %q", c.PublicH2Origin)
	}

	t.Setenv("PUBLIC_H2_ORIGIN", "https://meter.example")
	c, err = Load()
	if err != nil {
		t.Fatal(err)
	}
	if c.PublicH2Origin != "https://meter.example" {
		t.Fatalf("explicit PublicH2Origin = %q", c.PublicH2Origin)
	}
}

func TestLoadDefersValidation(t *testing.T) {
	t.Setenv("GM_ENABLE_H2", "true")
	c, err := Load()
	if err != nil {
		t.Fatal(err)
	}
	if err := c.Validate(); err == nil {
		t.Fatal("incomplete environment config validated")
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
	c.EnableH1TLS = true
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
	c.PublicH1TLSOrigin = "http://meter.example"
	if err := c.Validate(); err == nil {
		t.Fatal("clear h1 TLS origin accepted")
	}
	c = Default()
	c.EnableH1TLS = true
	c.TLSCert, c.TLSKey = "c", "k"
	c.H1TLSAddr = c.H1Addr
	if err := c.Validate(); err == nil {
		t.Fatal("clear and TLS H1 listeners may not collide")
	}
	c = Default()
	c.EnableH2, c.EnableH3 = true, true
	c.TLSCert, c.TLSKey = "c", "k"
	c.H3Addr = c.H2Addr
	if err := c.Validate(); err == nil {
		t.Fatal("colliding TCP listeners accepted")
	}
}

func TestValidateRejectsNonOriginURLParts(t *testing.T) {
	for _, origin := range []string{
		"https://user:pass@meter.example",
		"https://meter.example?target=other",
		"https://meter.example?",
		"https://meter.example#fragment",
		"https://:7248",
	} {
		c := Default()
		c.PublicH2Origin = origin
		if err := c.Validate(); err == nil {
			t.Errorf("non-origin URL %q accepted", origin)
		}
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
	for _, k := range []string{"GM_ENABLE_H1_TLS", "GM_ENABLE_H2", "GM_ENABLE_H3", "GM_TLS_CERT", "GM_TLS_KEY", "PUBLIC_H1_ORIGIN", "PUBLIC_H1_TLS_ORIGIN", "PUBLIC_TLS_ORIGIN", "PUBLIC_H2_ORIGIN", "PUBLIC_H3_ORIGIN", "GM_TRUSTED_PROXIES"} {
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
	if c.H1Addr != ":7246" || c.H1TLSAddr != ":7247" || c.H2Addr != ":7248" || c.H3Addr != ":7249" || c.EnableH1TLS || c.EnableH2 || c.EnableH3 || len(c.TrustedProxies) != 0 {
		t.Fatalf("load defaults = %+v", c)
	}
}
