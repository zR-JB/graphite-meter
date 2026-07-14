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
	"time"

	"github.com/zR-JB/graphite-meter/go/internal/config"
)

func writeCertificate(t *testing.T, dir, name, host string, notBefore, notAfter time.Time) (string, string) {
	t.Helper()
	key, err := ecdsa.GenerateKey(elliptic.P256(), rand.Reader)
	if err != nil {
		t.Fatal(err)
	}
	tpl := &x509.Certificate{SerialNumber: big.NewInt(time.Now().UnixNano()), Subject: pkix.Name{CommonName: host}, DNSNames: []string{host}, NotBefore: notBefore, NotAfter: notAfter, KeyUsage: x509.KeyUsageDigitalSignature, ExtKeyUsage: []x509.ExtKeyUsage{x509.ExtKeyUsageServerAuth}}
	der, err := x509.CreateCertificate(rand.Reader, tpl, tpl, &key.PublicKey, key)
	if err != nil {
		t.Fatal(err)
	}
	certPath, keyPath := filepath.Join(dir, name+".crt"), filepath.Join(dir, name+".key")
	if err := os.WriteFile(certPath, pem.EncodeToMemory(&pem.Block{Type: "CERTIFICATE", Bytes: der}), 0644); err != nil {
		t.Fatal(err)
	}
	keyDER, _ := x509.MarshalPKCS8PrivateKey(key)
	if err := os.WriteFile(keyPath, pem.EncodeToMemory(&pem.Block{Type: "PRIVATE KEY", Bytes: keyDER}), 0600); err != nil {
		t.Fatal(err)
	}
	return certPath, keyPath
}

func tlsTestConfig(cert, key string) *config.Config {
	c := config.Default()
	c.EnableH2 = true
	c.TLSCert, c.TLSKey = cert, key
	c.PublicH2Origin = "https://meter.example:7248"
	return &c
}

func TestCertificateValidation(t *testing.T) {
	now := time.Now()
	dir := t.TempDir()
	validCert, validKey := writeCertificate(t, dir, "valid", "meter.example", now.Add(-time.Hour), now.Add(24*time.Hour))
	if _, err := newCertificateManager(tlsTestConfig(validCert, validKey)); err != nil {
		t.Fatalf("valid certificate: %v", err)
	}
	for _, tc := range []struct {
		name, host    string
		before, after time.Time
	}{
		{"expired", "meter.example", now.Add(-2 * time.Hour), now.Add(-time.Hour)},
		{"future", "meter.example", now.Add(time.Hour), now.Add(2 * time.Hour)},
		{"hostname", "other.example", now.Add(-time.Hour), now.Add(time.Hour)},
	} {
		t.Run(tc.name, func(t *testing.T) {
			cert, key := writeCertificate(t, dir, tc.name, tc.host, tc.before, tc.after)
			if _, err := newCertificateManager(tlsTestConfig(cert, key)); err == nil {
				t.Fatal("invalid certificate accepted")
			}
		})
	}
	_, otherKey := writeCertificate(t, dir, "other", "meter.example", now.Add(-time.Hour), now.Add(time.Hour))
	if _, err := newCertificateManager(tlsTestConfig(validCert, otherKey)); err == nil {
		t.Fatal("mismatched key accepted")
	}
}

func TestCertificateValidationIgnoresExternalOrigins(t *testing.T) {
	now := time.Now()
	dir := t.TempDir()
	cert, key := writeCertificate(t, dir, "quic", "quic.example", now.Add(-time.Hour), now.Add(time.Hour))
	cfg := config.Default()
	cfg.EnableH3 = true
	cfg.TLSCert, cfg.TLSKey = cert, key
	cfg.PublicH2Origin = "https://speed.example"
	cfg.PublicH3Origin = "https://quic.example"
	if _, err := newCertificateManager(&cfg); err != nil {
		t.Fatalf("native H3 certificate rejected for external H2 origin: %v", err)
	}
}

func TestCertificateReloadKeepsLastValid(t *testing.T) {
	now := time.Now()
	dir := t.TempDir()
	cert, key := writeCertificate(t, dir, "live", "meter.example", now.Add(-time.Hour), now.Add(time.Hour))
	m, err := newCertificateManager(tlsTestConfig(cert, key))
	if err != nil {
		t.Fatal(err)
	}
	previous := m.current.Load()
	if err := os.WriteFile(cert, []byte("incomplete renewal"), 0644); err != nil {
		t.Fatal(err)
	}
	if err := m.reload(now); err == nil {
		t.Fatal("incomplete renewal accepted")
	}
	if m.current.Load() != previous {
		t.Fatal("last valid certificate replaced")
	}
}
