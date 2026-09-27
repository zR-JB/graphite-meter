package server

import (
	"crypto/ecdsa"
	"crypto/elliptic"
	"crypto/rand"
	"crypto/x509"
	"crypto/x509/pkix"
	"encoding/pem"
	"math/big"
	"os"
	"path/filepath"
	"testing"
	"testing/synctest"
	"time"

	"github.com/zR-JB/graphite-meter/go/internal/config"
)

func writeCertificate(t *testing.T, dir, name, host string, notBefore, notAfter time.Time) (string, string) {
	t.Helper()
	key, err := ecdsa.GenerateKey(elliptic.P256(), rand.Reader)
	if err != nil {
		t.Fatal(err)
	}
	tpl := &x509.Certificate{SerialNumber: big.NewInt(time.Now().UnixNano()), Subject: pkix.Name{CommonName: host},
		DNSNames: []string{host}, NotBefore: notBefore, NotAfter: notAfter, KeyUsage: x509.KeyUsageDigitalSignature,
		ExtKeyUsage: []x509.ExtKeyUsage{x509.ExtKeyUsageServerAuth}}
	der, err := x509.CreateCertificate(rand.Reader, tpl, tpl, &key.PublicKey, key)
	if err != nil {
		t.Fatal(err)
	}
	certPath, keyPath := filepath.Join(dir, name+".crt"), filepath.Join(dir, name+".key")
	if err := os.WriteFile(certPath, pem.EncodeToMemory(&pem.Block{Type: "CERTIFICATE", Bytes: der}),
		0644); err != nil {
		t.Fatal(err)
	}
	keyDER, _ := x509.MarshalPKCS8PrivateKey(key)
	if err := os.WriteFile(keyPath, pem.EncodeToMemory(&pem.Block{Type: "PRIVATE KEY", Bytes: keyDER}),
		0600); err != nil {
		t.Fatal(err)
	}
	return certPath, keyPath
}

func tlsTestConfig(cert, key string) *config.Config {
	c := config.Default()
	c.Native.H2 = ":7248"
	c.TLSCert, c.TLSKey = cert, key
	c.NativePublic.H2 = "https://meter.example:7248"
	return &c
}

func TestCertificateValidation(t *testing.T) {
	now := time.Now()
	dir := t.TempDir()
	// Only native origins must match: an external page origin and a separately named H3 origin do not.
	external := func(c *config.Config) {
		c.Native.H2, c.NativePublic.H2 = "", ""
		c.Native.H3, c.NativePublic.H3 = ":7249", "https://quic.example"
		c.Public.Both = []string{"https://speed.example"}
	}
	for _, tc := range []struct {
		name, host    string
		before, after time.Time
		tune          func(*config.Config)
		valid         bool
	}{
		{"valid", "meter.example", now.Add(-time.Hour), now.Add(24 * time.Hour), nil, true},
		{"expired", "meter.example", now.Add(-2 * time.Hour), now.Add(-time.Hour), nil, false},
		{"future", "meter.example", now.Add(time.Hour), now.Add(2 * time.Hour), nil, false},
		{"hostname", "other.example", now.Add(-time.Hour), now.Add(time.Hour), nil, false},
		{"external origins", "quic.example", now.Add(-time.Hour), now.Add(time.Hour), external, true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			cfg := tlsTestConfig(writeCertificate(t, dir, tc.name, tc.host, tc.before, tc.after))
			if tc.tune != nil {
				tc.tune(cfg)
			}
			if _, err := newCertificateManager(cfg); (err == nil) != tc.valid {
				t.Fatalf("accepted = %t, want %t: %v", err == nil, tc.valid, err)
			}
		})
	}
	cert, _ := writeCertificate(t, dir, "cert", "meter.example", now.Add(-time.Hour), now.Add(time.Hour))
	_, otherKey := writeCertificate(t, dir, "other", "meter.example", now.Add(-time.Hour), now.Add(time.Hour))
	if _, err := newCertificateManager(tlsTestConfig(cert, otherKey)); err == nil {
		t.Fatal("mismatched key accepted")
	}
}

func TestCertificateRenewal(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		now := time.Now()
		dir := t.TempDir()
		cert, key := writeCertificate(t, dir, "live", "meter.example", now.Add(-time.Hour), now.Add(time.Hour))
		m, err := newCertificateManager(tlsTestConfig(cert, key))
		if err != nil {
			t.Fatal(err)
		}
		go m.run(t.Context())
		first := m.current.Load()
		if err := os.WriteFile(cert, []byte("incomplete renewal"), 0644); err != nil {
			t.Fatal(err)
		}
		time.Sleep(certPollInterval)
		synctest.Wait()
		if m.current.Load() != first {
			t.Fatal("an incomplete renewal replaced the last valid certificate")
		}
		renewed := now.Add(48 * time.Hour).Truncate(time.Second)
		writeCertificate(t, dir, "live", "meter.example", now.Add(-time.Hour), renewed)
		time.Sleep(certPollInterval)
		synctest.Wait()
		if got := m.current.Load(); !got.Leaf.NotAfter.Equal(renewed) {
			t.Fatalf("serving a certificate until %v, want the renewal until %v", got.Leaf.NotAfter, renewed)
		}
	})
}
