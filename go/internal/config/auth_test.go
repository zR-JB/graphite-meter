package config

import "testing"

func passwordAuthConfig(t *testing.T) Config {
	t.Helper()
	c := Default()
	c.AdvertiseAllNative = false
	c.Auth = AuthConfig{Mode: "password", PublicURL: "https://meter.example", PasswordHash: "hash", OIDCProviderName: "Authelia"}
	c.Public.Throughput = []string{"https://meter.example"}
	return c
}

func TestValidateRejectsInconsistentAuthConfiguration(t *testing.T) {
	tests := []struct {
		name   string
		mutate func(*Config)
	}{
		{"clear public URL", func(c *Config) { c.Auth.PublicURL = "http://meter.example" }},
		{"explicit default port", func(c *Config) { c.Auth.PublicURL = "https://meter.example:443" }},
		{"dual password sources", func(c *Config) { c.Auth.PasswordHashFile = "/secret" }},
		{"clear advertised listener", func(c *Config) { c.AdvertiseAllNative = true }},
		{"alternate hostname", func(c *Config) { c.Public.Throughput = []string{"https://other.example"} }},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			c := passwordAuthConfig(t)
			tt.mutate(&c)
			if err := c.Validate(); err == nil {
				t.Fatal("configuration unexpectedly accepted")
			}
		})
	}
}

func TestValidateAcceptsCompletePasswordAuth(t *testing.T) {
	c := passwordAuthConfig(t)
	if err := c.Validate(); err != nil {
		t.Fatal(err)
	}
}

func TestAuthSettingsRejectedWhenOff(t *testing.T) {
	c := Default()
	c.Auth.PublicURL = "https://meter.example"
	if err := c.Validate(); err == nil {
		t.Fatal("auth setting accepted in off mode")
	}
}

func TestExplicitDefaultAuthSettingRejectedWhenOff(t *testing.T) {
	t.Setenv("GM_AUTH_OIDC_PROVIDER_NAME", "Authelia")
	c, err := Load()
	if err != nil {
		t.Fatal(err)
	}
	if err := c.Validate(); err == nil {
		t.Fatal("explicit auth setting accepted in off mode")
	}
}

func TestExplicitOffModeIsAccepted(t *testing.T) {
	t.Setenv("GM_AUTH_MODE", "off")
	c, err := Load()
	if err != nil {
		t.Fatal(err)
	}
	if err := c.Validate(); err != nil {
		t.Fatal(err)
	}
}
