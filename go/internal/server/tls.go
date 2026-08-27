package server

import (
	"bytes"
	"context"
	"crypto/tls"
	"crypto/x509"
	"errors"
	"fmt"
	"log"
	"net/url"
	"os"
	"sync/atomic"
	"time"

	"github.com/zR-JB/graphite-meter/go/internal/config"
)

const certPollInterval = time.Minute

type certificateManager struct {
	cfg     *config.Config
	current atomic.Pointer[tls.Certificate]
}

func newCertificateManager(cfg *config.Config) (*certificateManager, error) {
	m := &certificateManager{cfg: cfg}
	if err := m.reload(time.Now()); err != nil {
		return nil, err
	}
	if info, err := os.Stat(cfg.TLSKey); err == nil && info.Mode().Perm()&0077 != 0 {
		log.Printf("[gm:tls] warning: private key %s permissions are %04o; remove group/other access", cfg.TLSKey, info.Mode().Perm())
	}
	return m, nil
}

func (m *certificateManager) reload(now time.Time) error {
	cert, err := tls.LoadX509KeyPair(m.cfg.TLSCert, m.cfg.TLSKey)
	if err != nil {
		return fmt.Errorf("load matching TLS certificate/key: %w", err)
	}
	if len(cert.Certificate) == 0 {
		return errors.New("TLS certificate chain is empty")
	}
	leaf, err := x509.ParseCertificate(cert.Certificate[0])
	if err != nil {
		return fmt.Errorf("parse TLS leaf certificate: %w", err)
	}
	if now.Before(leaf.NotBefore) {
		return fmt.Errorf("TLS certificate is not valid before %s", leaf.NotBefore.Format(time.RFC3339))
	}
	if !now.Before(leaf.NotAfter) {
		return fmt.Errorf("TLS certificate expired at %s", leaf.NotAfter.Format(time.RFC3339))
	}
	for _, public := range []struct {
		enabled bool
		origin  string
	}{{m.cfg.Native.H1TLS != "", m.cfg.NativePublic.H1TLS}, {m.cfg.Native.H2 != "", m.cfg.NativePublic.H2}, {m.cfg.Native.H3 != "", m.cfg.NativePublic.H3}} {
		if !public.enabled || public.origin == "" {
			continue
		}
		// Config validation guarantees every public origin parses.
		u, _ := url.Parse(public.origin)
		if err := leaf.VerifyHostname(u.Hostname()); err != nil {
			return fmt.Errorf("TLS certificate incompatible with %s: %w", u.Hostname(), err)
		}
	}
	previous := m.current.Load()
	changed := previous == nil || previous.Leaf == nil || !bytes.Equal(previous.Leaf.Raw, leaf.Raw)
	cert.Leaf = leaf
	m.current.Store(new(cert))
	if changed {
		remaining := leaf.NotAfter.Sub(now)
		log.Printf("[gm:tls] certificate loaded; expires at %s", leaf.NotAfter.Format(time.RFC3339))
		if remaining < 30*24*time.Hour {
			log.Printf("[gm:tls] warning: certificate expires in %s", remaining.Round(time.Hour))
		}
	}
	return nil
}

func (m *certificateManager) tlsConfig(nextProtos ...string) *tls.Config {
	return &tls.Config{
		MinVersion:     tls.VersionTLS13,
		NextProtos:     nextProtos,
		GetCertificate: func(*tls.ClientHelloInfo) (*tls.Certificate, error) { return m.current.Load(), nil },
	}
}

func (m *certificateManager) run(ctx context.Context) {
	ticker := time.Tick(certPollInterval)
	for {
		select {
		case <-ctx.Done():
			return
		case now := <-ticker:
			if err := m.reload(now); err != nil {
				log.Printf("[gm:tls] renewal rejected; keeping last valid certificate: %v", err)
			}
		}
	}
}
