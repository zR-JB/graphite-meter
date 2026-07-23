package config

import (
	"os"
	"testing"
)

func clearConfigEnv(t *testing.T) {
	t.Helper()
	for _, key := range []string{"GM_H1_ADDR", "GM_H1_TLS_ADDR", "GM_H2_ADDR", "GM_H3_ADDR", "GM_TLS_CERT", "GM_TLS_KEY", "GM_H1_PUBLIC_ORIGIN", "GM_H1_TLS_PUBLIC_ORIGIN", "GM_H2_PUBLIC_ORIGIN", "GM_H3_PUBLIC_ORIGIN", "GM_ADVERTISED_NATIVE_ENDPOINTS", "GM_PUBLIC_ORIGINS", "GM_PUBLIC_THROUGHPUT_ORIGINS", "GM_PUBLIC_LATENCY_ORIGINS", "GM_SERVER_NAME", "GM_SERVER_LOCATION", "GM_VERBOSE", "GM_TRUSTED_PROXIES"} {
		t.Setenv(key, "")
		_ = os.Unsetenv(key)
	}
}

func TestDefaultIsNativeH1Only(t *testing.T) {
	c := Default()
	if c.Native.H1 != ":7246" || c.Native.H1TLS != "" || c.Native.H2 != "" || c.Native.H3 != "" || !c.AdvertiseAllNative {
		t.Fatalf("native listeners = %+v", c.Native)
	}
	if err := c.Validate(); err != nil {
		t.Fatal(err)
	}
}

func TestLoadEndpointConfiguration(t *testing.T) {
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
		t.Fatalf("advertised = %#v", c.AdvertisedNative)
	}
	if c.NativePublic.H2 != "https://h2.example" || len(c.Public.Both) != 2 {
		t.Fatalf("public = %+v native = %+v", c.Public, c.NativePublic)
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

func TestValidateProxyOnly(t *testing.T) {
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

func TestValidateOriginsAndTLS(t *testing.T) {
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

func TestParseAdvertisedNative(t *testing.T) {
	all, err := ParseAdvertisedNative("all")
	if err != nil || len(all) != 4 {
		t.Fatalf("all = %#v, %v", all, err)
	}
	none, err := ParseAdvertisedNative("none")
	if err != nil || len(none) != 0 {
		t.Fatalf("none = %#v, %v", none, err)
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
