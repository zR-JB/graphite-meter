package config

import (
	"net/netip"
	"reflect"
	"testing"
)

// envKeys lists every environment variable Load reads.
var envKeys = []string{
	"GM_H1_ADDR",
	"GM_H3_ADDR",
	"GM_TLS_CERT",
	"GM_TLS_KEY", //gitleaks:allow -- env var name, not a secret value
	"GM_ADVERTISE_H3",
	"PUBLIC_H1_ORIGIN",
	"PUBLIC_TLS_ORIGIN",
	"PUBLIC_H3_ORIGIN",
	"GM_SERVER_NAME",
	"GM_SERVER_LOCATION",
	"GM_VERBOSE",
	"GM_TRUSTED_PROXIES",
}

// clearEnv sets every env var Load reads to "", scoped to the test via
// t.Setenv, guaranteeing a clean slate regardless of the ambient environment.
func clearEnv(t *testing.T) {
	t.Helper()
	for _, k := range envKeys {
		t.Setenv(k, "")
	}
}

func TestDefault(t *testing.T) {
	want := Config{
		H1Addr:        ":8765",
		H3Addr:        ":8443",
		AdvertiseH3:   false,
		ServerName:    "graphite-meter",
		EngineVersion: EngineVersion,
	}
	if got := Default(); !reflect.DeepEqual(got, want) {
		t.Errorf("Default() = %+v, want %+v", got, want)
	}
}

func TestLoad_NoEnvMatchesDefault(t *testing.T) {
	clearEnv(t)
	got, err := Load()
	if err != nil {
		t.Fatal(err)
	}
	if want := Default(); !reflect.DeepEqual(got, want) {
		t.Errorf("Load() = %+v, want Default() = %+v", got, want)
	}
}

func TestLoad_StringOverrides(t *testing.T) {
	clearEnv(t)
	t.Setenv("GM_H1_ADDR", ":9999")
	t.Setenv("GM_H3_ADDR", ":9443")
	t.Setenv("GM_TLS_CERT", "/tmp/cert.pem")
	t.Setenv("GM_TLS_KEY", "/tmp/key.pem")
	t.Setenv("PUBLIC_H1_ORIGIN", "http://example.com")
	t.Setenv("PUBLIC_TLS_ORIGIN", "https://example.com")
	t.Setenv("PUBLIC_H3_ORIGIN", "https://example.com:8443")
	t.Setenv("GM_SERVER_NAME", "test-server")
	t.Setenv("GM_SERVER_LOCATION", "test-location")

	c, err := Load()
	if err != nil {
		t.Fatal(err)
	}

	if c.H1Addr != ":9999" {
		t.Errorf("H1Addr = %q, want %q", c.H1Addr, ":9999")
	}
	if c.H3Addr != ":9443" {
		t.Errorf("H3Addr = %q, want %q", c.H3Addr, ":9443")
	}
	if c.TLSCert != "/tmp/cert.pem" {
		t.Errorf("TLSCert = %q, want %q", c.TLSCert, "/tmp/cert.pem")
	}
	if c.TLSKey != "/tmp/key.pem" {
		t.Errorf("TLSKey = %q, want %q", c.TLSKey, "/tmp/key.pem")
	}
	if c.PublicH1Origin != "http://example.com" {
		t.Errorf("PublicH1Origin = %q, want %q", c.PublicH1Origin, "http://example.com")
	}
	if c.PublicTLSOrigin != "https://example.com" {
		t.Errorf("PublicTLSOrigin = %q, want %q", c.PublicTLSOrigin, "https://example.com")
	}
	if c.PublicH3Origin != "https://example.com:8443" {
		t.Errorf("PublicH3Origin = %q, want %q", c.PublicH3Origin, "https://example.com:8443")
	}
	if c.ServerName != "test-server" {
		t.Errorf("ServerName = %q, want %q", c.ServerName, "test-server")
	}
	if c.ServerLocation != "test-location" {
		t.Errorf("ServerLocation = %q, want %q", c.ServerLocation, "test-location")
	}
}

func TestLoad_AdvertiseH3(t *testing.T) {
	cases := []struct {
		env  string
		want bool
	}{
		{"1", true},
		{"true", true},
		{"yes", false},
	}
	for _, tc := range cases {
		t.Run(tc.env, func(t *testing.T) {
			clearEnv(t)
			t.Setenv("GM_ADVERTISE_H3", tc.env)
			c, err := Load()
			if err != nil {
				t.Fatal(err)
			}
			if got := c.AdvertiseH3; got != tc.want {
				t.Errorf("AdvertiseH3 with GM_ADVERTISE_H3=%q = %v, want %v", tc.env, got, tc.want)
			}
		})
	}
}

func TestLoad_Verbose(t *testing.T) {
	cases := []struct {
		env  string
		want bool
	}{
		{"1", true},
		{"true", true},
		{"yes", false},
	}
	for _, tc := range cases {
		t.Run(tc.env, func(t *testing.T) {
			clearEnv(t)
			t.Setenv("GM_VERBOSE", tc.env)
			c, err := Load()
			if err != nil {
				t.Fatal(err)
			}
			if got := c.Verbose; got != tc.want {
				t.Errorf("Verbose with GM_VERBOSE=%q = %v, want %v", tc.env, got, tc.want)
			}
		})
	}
}

func TestLoad_EmptyEnvDoesNotOverride(t *testing.T) {
	clearEnv(t)
	// Explicitly empty (as opposed to unset) must fall through to defaults.
	t.Setenv("GM_H1_ADDR", "")
	t.Setenv("GM_ADVERTISE_H3", "")

	c, err := Load()
	if err != nil {
		t.Fatal(err)
	}
	if c.H1Addr != Default().H1Addr {
		t.Errorf("H1Addr = %q, want default %q", c.H1Addr, Default().H1Addr)
	}
	if c.AdvertiseH3 != Default().AdvertiseH3 {
		t.Errorf("AdvertiseH3 = %v, want default %v", c.AdvertiseH3, Default().AdvertiseH3)
	}
}

func TestLoadTrustedProxies(t *testing.T) {
	clearEnv(t)
	t.Setenv("GM_TRUSTED_PROXIES", "127.0.0.1/8, ::1/128, 10.0.0.0/8")
	c, err := Load()
	if err != nil {
		t.Fatal(err)
	}
	want := []netip.Prefix{
		netip.MustParsePrefix("127.0.0.1/8"),
		netip.MustParsePrefix("::1/128"),
		netip.MustParsePrefix("10.0.0.0/8"),
	}
	if !reflect.DeepEqual(c.TrustedProxies, want) {
		t.Fatalf("TrustedProxies = %v, want %v", c.TrustedProxies, want)
	}
}

func TestLoadRejectsInvalidTrustedProxy(t *testing.T) {
	clearEnv(t)
	t.Setenv("GM_TRUSTED_PROXIES", "127.0.0.1/8,not-a-cidr")
	if _, err := Load(); err == nil {
		t.Fatal("Load() error = nil, want invalid CIDR error")
	}
}
