package config

import (
	"os"
	"strings"
	"testing"
	"time"
)

func clearConfigEnv(t *testing.T) {
	t.Helper()
	for _, key := range []string{"GM_H1_ADDR", "GM_H1_TLS_ADDR", "GM_H2_ADDR", "GM_H3_ADDR", "GM_TLS_CERT", "GM_TLS_KEY", "GM_H1_PUBLIC_ORIGIN", "GM_H1_TLS_PUBLIC_ORIGIN", "GM_H2_PUBLIC_ORIGIN", "GM_H3_PUBLIC_ORIGIN", "GM_ADVERTISED_NATIVE_ENDPOINTS", "GM_PUBLIC_ORIGINS", "GM_PUBLIC_THROUGHPUT_ORIGINS", "GM_PUBLIC_LATENCY_ORIGINS", "GM_SERVER_NAME", "GM_SERVER_LOCATION", "GM_VERBOSE", "GM_TRUSTED_PROXIES"} {
		unsetEnv(t, key)
	}
}

func unsetEnv(t *testing.T, key string) {
	t.Helper()
	t.Setenv(key, "")
	_ = os.Unsetenv(key)
}

func TestDefaultIsNativeH1Only(t *testing.T) {
	c := Default()
	if c.Native.H1 != ":7246" || c.Native.H1TLS != "" || c.Native.H2 != "" || c.Native.H3 != "" || !c.AdvertiseAllNative {
		t.Fatalf("native listeners = %+v, advertiseAll = %v, want only H1 on :7246 with advertiseAll true", c.Native, c.AdvertiseAllNative)
	}
	if err := c.Validate(); err != nil {
		t.Fatal(err)
	}
}

func TestLoadAppliesEndpointEnvironment(t *testing.T) {
	clearConfigEnv(t)
	t.Setenv("GM_H1_TLS_ADDR", ":7247")
	t.Setenv("GM_H2_ADDR", ":7248")
	t.Setenv("GM_H3_ADDR", ":7249")
	t.Setenv("GM_TLS_CERT", "cert.pem")
	t.Setenv("GM_TLS_KEY", "key.pem")
	t.Setenv("GM_ADVERTISED_NATIVE_ENDPOINTS", "http1-tls,http2")
	t.Setenv("GM_H2_PUBLIC_ORIGIN", "https://h2.example")
	t.Setenv("GM_PUBLIC_ORIGINS", "self, https://meter.example")
	t.Setenv("GM_PUBLIC_THROUGHPUT_ORIGINS", "http://meter.example")
	c, err := Load()
	if err != nil {
		t.Fatal(err)
	}
	if !c.AdvertisedNative[NativeH1TLS] || !c.AdvertisedNative[NativeH2] || len(c.AdvertisedNative) != 2 {
		t.Fatalf("advertised = %#v, want exactly %q and %q", c.AdvertisedNative, NativeH1TLS, NativeH2)
	}
	if c.NativePublic.H2 != "https://h2.example" || len(c.Public.Both) != 2 {
		t.Fatalf("native = %+v, public = %+v, want H2 origin %q and 2 shared public origins", c.NativePublic, c.Public, "https://h2.example")
	}
	if err := c.Validate(); err != nil {
		t.Fatal(err)
	}
}

func TestValidateRejectsDisabledAdvertisement(t *testing.T) {
	c := Default()
	c.AdvertiseAllNative = false
	c.AdvertisedNative = map[string]bool{NativeH2: true}
	if err := c.Validate(); err == nil {
		t.Fatal("expected disabled endpoint error")
	}
}

func TestValidateAcceptsProxyOnlyDeployment(t *testing.T) {
	c := Default()
	c.AdvertiseAllNative = false
	c.AdvertisedNative = map[string]bool{}
	c.Public.Both = []string{"self"}
	if err := c.Validate(); err != nil {
		t.Fatal(err)
	}
}

func TestValidateRequiresThroughput(t *testing.T) {
	c := Default()
	c.AdvertiseAllNative = false
	c.AdvertisedNative = map[string]bool{}
	c.Public.Latency = []string{"self"}
	if err := c.Validate(); err == nil {
		t.Fatal("expected no throughput error")
	}
}

func TestValidateRejectsMissingTLSAndBadOrigins(t *testing.T) {
	c := Default()
	c.Native.H2 = ":7248"
	if err := c.Validate(); err == nil {
		t.Fatal("expected TLS error")
	}
	c.TLSCert, c.TLSKey = "cert", "key"
	c.NativePublic.H2 = "http://bad.example"
	if err := c.Validate(); err == nil {
		t.Fatal("expected scheme error")
	}
	c.NativePublic.H2 = "https://h2.example"
	c.Public.Both = []string{"ftp://bad.example"}
	if err := c.Validate(); err == nil {
		t.Fatal("expected public origin error")
	}
}

func TestValidateNormalizesOriginsBeforeDuplicateChecks(t *testing.T) {
	t.Run("deterministic protocols", func(t *testing.T) {
		c := Default()
		c.Native.H1TLS, c.Native.H2 = ":7247", ":7248"
		c.TLSCert, c.TLSKey = "cert", "key"
		c.NativePublic.H1TLS = "https://meter.example:443"
		c.NativePublic.H2 = "https://meter.example"
		if err := c.Validate(); err == nil {
			t.Fatal("expected equivalent deterministic origin error")
		}
	})
	t.Run("native and negotiated", func(t *testing.T) {
		c := Default()
		c.NativePublic.H1 = "http://meter.example:80"
		c.Public.Both = []string{"http://meter.example"}
		if err := c.Validate(); err == nil {
			t.Fatal("expected equivalent negotiated origin error")
		}
	})
}

func TestParseAdvertisedNativeResolvesAliasesAndRejectsUnknown(t *testing.T) {
	all, err := ParseAdvertisedNative("all")
	if err != nil || len(all) != 4 {
		t.Fatalf("ParseAdvertisedNative(\"all\") = %#v, %v, want 4 endpoints and no error", all, err)
	}
	none, err := ParseAdvertisedNative("none")
	if err != nil || len(none) != 0 {
		t.Fatalf("ParseAdvertisedNative(\"none\") = %#v, %v, want an empty set and no error", none, err)
	}
	if _, err := ParseAdvertisedNative("fictional"); err == nil {
		t.Fatal("expected unknown endpoint error")
	}
}

func TestTrustedProxiesRejectsDefaultRoute(t *testing.T) {
	for _, cidr := range []string{"0.0.0.0/0", "::/0", "10.0.0.0/8,0.0.0.0/0"} {
		clearConfigEnv(t)
		t.Setenv("GM_TRUSTED_PROXIES", cidr)
		if _, err := Load(); err == nil {
			t.Errorf("GM_TRUSTED_PROXIES=%q loaded without error; a default route must be rejected", cidr)
		}
	}
}

func TestTrustedProxiesMasksHostBits(t *testing.T) {
	clearConfigEnv(t)
	t.Setenv("GM_TRUSTED_PROXIES", "192.168.1.42/24")
	c, err := Load()
	if err != nil {
		t.Fatal(err)
	}
	if len(c.TrustedProxies) != 1 || c.TrustedProxies[0].String() != "192.168.1.0/24" {
		t.Fatalf("trusted proxies = %v, want the masked 192.168.1.0/24", c.TrustedProxies)
	}
}

func TestValidateBoundsTheSessionBudget(t *testing.T) {
	if c := Default(); c.MaxActiveSessions != 64 || c.MaxActiveSessions*4 != c.MaxActiveMeasurements {
		t.Fatalf("MaxActiveSessions = %d of %d, want a quarter of the pool", c.MaxActiveSessions, c.MaxActiveMeasurements)
	}
	for _, tc := range []struct {
		name   string
		mutate func(*Config)
	}{
		{"zero", func(c *Config) { c.MaxActiveSessions = 0 }},
		{"negative", func(c *Config) { c.MaxActiveSessions = -1 }},
		{"over the pool", func(c *Config) { c.MaxActiveSessions = c.MaxActiveMeasurements + 1 }},
		{"one client takes the whole budget", func(c *Config) { c.MaxActiveSessions, c.MaxSessionsPerClient = 4, 8 }},
	} {
		t.Run(tc.name, func(t *testing.T) {
			c := Default()
			tc.mutate(&c)
			if err := c.Validate(); err == nil {
				t.Fatalf("MaxActiveSessions=%d MaxSessionsPerClient=%d accepted", c.MaxActiveSessions, c.MaxSessionsPerClient)
			}
		})
	}
	c := Default()
	c.MaxActiveSessions, c.MaxSessionsPerClient = 8, 8
	if err := c.Validate(); err != nil {
		t.Fatalf("equal session budgets rejected: %v", err)
	}
}

func TestLoadReadsTheSessionBudgetFromTheEnvironment(t *testing.T) {
	clearConfigEnv(t)
	t.Setenv("GM_MAX_ACTIVE_SESSIONS", "40")
	c, err := Load()
	if err != nil {
		t.Fatal(err)
	}
	if c.MaxActiveSessions != 40 {
		t.Fatalf("MaxActiveSessions = %d, want 40", c.MaxActiveSessions)
	}
	if err := c.Validate(); err != nil {
		t.Fatal(err)
	}
}

func TestValidateRejectsNonPositiveSessionLimits(t *testing.T) {
	for _, tc := range []struct {
		name, want                        string
		activeSessions, sessionsPerClient int
	}{
		{"zero", "GM_MAX_ACTIVE_SESSIONS", 0, 0},
		{"negative", "GM_MAX_ACTIVE_SESSIONS", -1, -1},
		{"zero per client", "GM_MAX_SESSIONS_PER_CLIENT", 64, 0},
		{"negative per client", "GM_MAX_SESSIONS_PER_CLIENT", 64, -1},
	} {
		t.Run(tc.name, func(t *testing.T) {
			c := Default()
			c.MaxActiveSessions, c.MaxSessionsPerClient = tc.activeSessions, tc.sessionsPerClient
			err := c.Validate()
			if err == nil || !strings.Contains(err.Error(), tc.want) {
				t.Fatalf("Validate() = %v, want an error naming %s", err, tc.want)
			}
		})
	}
}

func TestValidateHoldsTheClientSessionBudgetInsideTheClientPool(t *testing.T) {
	c := Default()
	c.MaxSessionsPerClient = c.MaxActiveMeasurementsPerClient + 1
	if err := c.Validate(); err == nil {
		t.Fatalf("MaxSessionsPerClient=%d accepted over MaxActiveMeasurementsPerClient=%d", c.MaxSessionsPerClient, c.MaxActiveMeasurementsPerClient)
	}
}

// A session shorter than one operation cannot carry one: it ends mid-transfer.
func TestValidateRequiresASessionToOutlastAnOperation(t *testing.T) {
	c := Default()
	c.MaxSessionDuration = c.MaxOperationDuration - time.Second
	if err := c.Validate(); err == nil {
		t.Fatalf("MaxSessionDuration=%v accepted under MaxOperationDuration=%v", c.MaxSessionDuration, c.MaxOperationDuration)
	}
}

// Both documented duration variables, pinned by name: the loop that reads them is the only thing making them work.
func TestLoadReadsTheDurationsFromTheEnvironment(t *testing.T) {
	clearConfigEnv(t)
	t.Setenv("GM_MAX_OPERATION_DURATION", "90s")
	t.Setenv("GM_MAX_SESSION_DURATION", "3h")
	c, err := Load()
	if err != nil {
		t.Fatal(err)
	}
	if c.MaxOperationDuration != 90*time.Second || c.MaxSessionDuration != 3*time.Hour {
		t.Fatalf("durations = (%v, %v), want (90s, 3h)", c.MaxOperationDuration, c.MaxSessionDuration)
	}
	if err := c.Validate(); err != nil {
		t.Fatal(err)
	}
}

func TestDefaultSessionDurationIsTwoHours(t *testing.T) {
	if c := Default(); c.MaxSessionDuration != 2*time.Hour {
		t.Fatalf("MaxSessionDuration = %v, want 2h", c.MaxSessionDuration)
	}
}
