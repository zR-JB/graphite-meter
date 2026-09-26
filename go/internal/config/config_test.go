package config

import (
	"os"
	"strings"
	"testing"
	"time"
)

func unsetEnv(t *testing.T, key string) {
	t.Helper()
	t.Setenv(key, "")
	_ = os.Unsetenv(key)
}

func clearConfigEnv(t *testing.T) {
	t.Helper()
	for _, s := range new(Config).settings() {
		unsetEnv(t, s.env)
	}
	unsetEnv(t, "GM_SERVER_CATALOG")
	unsetEnv(t, "GM_SERVER_CATALOG_FILE")
}

func TestLoad(t *testing.T) {
	for _, tc := range []struct {
		name  string
		env   map[string]string
		check func(Config) bool
	}{
		{"endpoints", map[string]string{
			"GM_H1_TLS_ADDR": ":7247", "GM_H2_ADDR": ":7248", "GM_H3_ADDR": ":7249",
			"GM_TLS_CERT": "cert.pem", "GM_TLS_KEY": "key.pem",
			"GM_ADVERTISED_NATIVE_ENDPOINTS": "http1-tls,http2", "GM_H2_PUBLIC_ORIGIN": "https://h2.example",
			"GM_PUBLIC_ORIGINS": "self, https://meter.example", "GM_PUBLIC_THROUGHPUT_ORIGINS": "http://meter.example",
		}, func(c Config) bool {
			return len(c.AdvertisedNative) == 2 && c.AdvertisedNative[NativeH1TLS] && c.AdvertisedNative[NativeH2] &&
				c.NativePublic.H2 == "https://h2.example" && len(c.Public.Both) == 2
		}},
		{"every native endpoint", map[string]string{"GM_ADVERTISED_NATIVE_ENDPOINTS": "all"},
			func(c Config) bool { return c.AdvertisedNative == nil }},
		{"result history", map[string]string{"GM_RESULT_HISTORY_DEFAULT": "true"},
			func(c Config) bool { return c.ResultHistoryDefault }},
		{"proxy host bits masked", map[string]string{"GM_TRUSTED_PROXIES": "192.168.1.42/24"}, func(c Config) bool {
			return len(c.TrustedProxies) == 1 && c.TrustedProxies[0].String() == "192.168.1.0/24"
		}},
		{"budgets and durations", map[string]string{
			"GM_MAX_ACTIVE_SESSIONS": "40", "GM_MAX_OPERATION_DURATION": "90s", "GM_MAX_SESSION_DURATION": "3h",
		}, func(c Config) bool {
			return c.MaxActiveSessions == 40 && c.MaxOperationDuration == 90*time.Second && c.MaxSessionDuration == 3*time.Hour
		}},
		{"explicit off mode", map[string]string{"GM_AUTH_MODE": "off"}, func(c Config) bool { return !c.Auth.Explicit }},
		{"invalid boolean", map[string]string{"GM_RESULT_HISTORY_DEFAULT": "not-a-bool"}, nil},
		{"unknown native endpoint", map[string]string{"GM_ADVERTISED_NATIVE_ENDPOINTS": "fictional"}, nil},
		{"IPv4 default route", map[string]string{"GM_TRUSTED_PROXIES": "0.0.0.0/0"}, nil},
		{"IPv6 default route", map[string]string{"GM_TRUSTED_PROXIES": "::/0"}, nil},
		{"listed default route", map[string]string{"GM_TRUSTED_PROXIES": "10.0.0.0/8,0.0.0.0/0"}, nil},
	} {
		t.Run(tc.name, func(t *testing.T) {
			clearConfigEnv(t)
			for key, value := range tc.env {
				t.Setenv(key, value)
			}
			c, err := Load()
			if tc.check == nil {
				if err == nil {
					t.Fatal("invalid environment loaded")
				}
				return
			}
			if err != nil {
				t.Fatal(err)
			}
			if !tc.check(c) {
				t.Fatalf("loaded %+v", c)
			}
			if err := c.Validate(); err != nil {
				t.Fatal(err)
			}
		})
	}
}

func TestExplicitDefaultAuthSettingRejectedWhenOff(t *testing.T) {
	clearConfigEnv(t)
	t.Setenv("GM_AUTH_OIDC_PROVIDER_NAME", "Authelia")
	c, err := Load()
	if err != nil {
		t.Fatal(err)
	}
	if err := c.Validate(); err == nil {
		t.Fatal("explicit auth setting accepted in off mode")
	}
}

func passwordAuth(c *Config) {
	c.AdvertisedNative = map[string]bool{}
	c.Auth = AuthConfig{Mode: "password", PublicURL: "https://meter.example", PasswordHash: "hash",
		OIDCProviderName: "Authelia"}
	c.Public.Throughput = []string{"https://meter.example"}
}

func TestValidate(t *testing.T) {
	tls := func(c *Config) { c.Native.H1TLS, c.Native.H2, c.TLSCert, c.TLSKey = ":7247", ":7248", "cert", "key" }
	for _, tc := range []struct {
		name, want string // want is a fragment of the error, or empty when the configuration is valid
		mutate     func(*Config)
	}{
		{"default", "", func(*Config) {}},
		{"proxy only", "", func(c *Config) { c.AdvertisedNative, c.Public.Both = map[string]bool{}, []string{"self"} }},
		{"equal session budgets", "", func(c *Config) { c.MaxActiveSessions, c.MaxSessionsPerClient = 8, 8 }},
		{"password auth", "", passwordAuth},
		{"disabled advertisement", "disabled endpoint", func(c *Config) {
			c.AdvertisedNative = map[string]bool{NativeH2: true}
		}},
		{"no throughput", "no throughput", func(c *Config) {
			c.AdvertisedNative, c.Public.Latency = map[string]bool{}, []string{"self"}
		}},
		{"missing TLS", "GM_TLS_CERT", func(c *Config) { c.Native.H2 = ":7248" }},
		{"native scheme", "GM_H2_PUBLIC_ORIGIN", func(c *Config) { tls(c); c.NativePublic.H2 = "http://bad.example" }},
		{"public origin", "GM_PUBLIC_ORIGINS", func(c *Config) { c.Public.Both = []string{"ftp://bad.example"} }},
		{"shared listener", "must differ", func(c *Config) { tls(c); c.Native.H2 = c.Native.H1TLS }},
		{"equivalent deterministic origins", "multiple deterministic", func(c *Config) {
			tls(c)
			c.NativePublic.H1TLS, c.NativePublic.H2 = "https://meter.example:443", "https://meter.example"
		}},
		{"native and negotiated origin", "both native", func(c *Config) {
			c.NativePublic.H1, c.Public.Both = "http://meter.example:80", []string{"http://meter.example"}
		}},
		{"zero sessions", "GM_MAX_ACTIVE_SESSIONS", func(c *Config) { c.MaxActiveSessions = 0 }},
		{"negative client sessions", "GM_MAX_SESSIONS_PER_CLIENT", func(c *Config) { c.MaxSessionsPerClient = -1 }},
		{"sessions over the pool", "GM_MAX_ACTIVE_SESSIONS", func(c *Config) {
			c.MaxActiveSessions = c.MaxActiveMeasurements + 1
		}},
		{"one client takes every session", "GM_MAX_SESSIONS_PER_CLIENT", func(c *Config) {
			c.MaxActiveSessions, c.MaxSessionsPerClient = 4, 8
		}},
		{"client sessions over the client pool", "GM_MAX_SESSIONS_PER_CLIENT", func(c *Config) {
			c.MaxSessionsPerClient = c.MaxActiveMeasurementsPerClient + 1
		}},
		{"session shorter than an operation", "GM_MAX_SESSION_DURATION", func(c *Config) {
			c.MaxSessionDuration = c.MaxOperationDuration - time.Second
		}},
		{"auth setting while off", "GM_AUTH_MODE", func(c *Config) { c.Auth.PublicURL = "https://meter.example" }},
		{"clear public URL", "GM_AUTH_PUBLIC_URL", func(c *Config) {
			passwordAuth(c)
			c.Auth.PublicURL = "http://meter.example"
		}},
		{"explicit default port", "GM_AUTH_PUBLIC_URL", func(c *Config) {
			passwordAuth(c)
			c.Auth.PublicURL = "https://meter.example:443"
		}},
		{"two password sources", "mutually exclusive", func(c *Config) {
			passwordAuth(c)
			c.Auth.PasswordHashFile = "/secret"
		}},
		{"clear listener advertised", "clear HTTP/1.1", func(c *Config) { passwordAuth(c); c.AdvertisedNative = nil }},
		{"alternate hostname", "canonical authentication hostname", func(c *Config) {
			passwordAuth(c)
			c.Public.Throughput = []string{"https://other.example"}
		}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			c := Default()
			tc.mutate(&c)
			err := c.Validate()
			if tc.want == "" && err != nil || tc.want != "" && (err == nil || !strings.Contains(err.Error(), tc.want)) {
				t.Fatalf("Validate() = %v, want %q", err, tc.want)
			}
		})
	}
}
