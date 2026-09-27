package server

import (
	"context"
	"errors"
	"fmt"
	"maps"
	"net"
	"net/http"
	"strings"
	"testing"
	"time"

	"github.com/zR-JB/graphite-meter/go/internal/config"
)

// testListenerSockets reserves the exact sockets an integration test will hand to listener assembly.
type testListenerSockets struct {
	t   *testing.T
	tcp map[string]net.Listener
	udp map[string]net.PacketConn
}

func newTestListenerSockets(t *testing.T) *testListenerSockets {
	t.Helper()
	s := &testListenerSockets{t: t, tcp: make(map[string]net.Listener), udp: make(map[string]net.PacketConn)}
	t.Cleanup(func() {
		for ln := range maps.Values(s.tcp) {
			_ = ln.Close()
		}
		for pc := range maps.Values(s.udp) {
			_ = pc.Close()
		}
	})
	return s
}

func (s *testListenerSockets) reserveTCP() string {
	s.t.Helper()
	ln, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		s.t.Fatalf("reserve TCP listener: %v", err)
	}
	addr := ln.Addr().String()
	s.tcp[addr] = ln
	return addr
}

func (s *testListenerSockets) reserveH3() string {
	s.t.Helper()
	for range 32 {
		ln, err := net.Listen("tcp", "127.0.0.1:0")
		if err != nil {
			s.t.Fatalf("reserve H3 TCP listener: %v", err)
		}
		addr := ln.Addr().String()
		pc, err := net.ListenPacket("udp", addr)
		if err != nil {
			_ = ln.Close()
			continue
		}
		s.tcp[addr] = ln
		s.udp[addr] = pc
		return addr
	}
	s.t.Fatal("could not reserve a shared TCP/UDP H3 port")
	return ""
}

// An unreserved address fails like a taken port, so no test ever binds a configured default such as :7246.
func (s *testListenerSockets) listenTCP(addr string) (net.Listener, error) {
	if ln, ok := s.tcp[addr]; ok {
		delete(s.tcp, addr)
		return ln, nil
	}
	return nil, fmt.Errorf("no reserved TCP listener for %s", addr)
}

func (s *testListenerSockets) listenUDP(addr string) (net.PacketConn, error) {
	if pc, ok := s.udp[addr]; ok {
		delete(s.udp, addr)
		return pc, nil
	}
	return nil, fmt.Errorf("no reserved UDP socket for %s", addr)
}

func runTestTLS(t *testing.T) (string, string) {
	t.Helper()
	return writeCertificate(t, t.TempDir(), "srv", "127.0.0.1",
		time.Now().Add(-time.Hour), time.Now().Add(time.Hour))
}

// serveBuild assembles cfg's listeners on sockets under a test certificate and serves them until the test ends.
func serveBuild(t *testing.T, cfg *config.Config, sockets listenerSockets, shape func(*endpoints)) *listenerBuild {
	t.Helper()
	cfg.TLSCert, cfg.TLSKey = runTestTLS(t)
	ctx, cancel := context.WithCancel(t.Context())
	t.Cleanup(cancel)
	build, err := newListenerBuild(ctx, cfg, sockets)
	if err != nil {
		t.Fatalf("build listeners: %v", err)
	}
	if shape != nil {
		shape(build.e)
	}
	if err := build.assemble(); err != nil {
		t.Fatalf("assemble listeners: %v", err)
	}
	startServices(t, build.services)
	return build
}

// startServices serves until the test ends, then cuts every connection at once; runServices pins the drain.
func startServices(t *testing.T, services []service) {
	cut, cancel := context.WithCancel(context.Background())
	cancel()
	for _, svc := range services {
		go func() { _ = svc.run() }()
		t.Cleanup(func() { _ = svc.stop(cut) })
	}
}

// startListeners runs the listeners tune reserves until the test ends.
func startListeners(t *testing.T, tune func(*config.Config, *testListenerSockets),
	shape func(*endpoints)) (*config.Config, *listenerBuild) {
	t.Helper()
	sockets := newTestListenerSockets(t)
	cfg := config.Default()
	tune(&cfg, sockets)
	return &cfg, serveBuild(t, &cfg, sockets, shape)
}

// runUntilCancel starts Run in the background and returns a stop function that cancels it and asserts a clean (nil).
func runUntilCancel(t *testing.T, cfg *config.Config, sockets listenerSockets) func() {
	t.Helper()
	ctx, cancel := context.WithCancel(t.Context())
	done := make(chan error, 1)
	go func() { done <- runWithSockets(ctx, cfg, sockets) }()
	return func() {
		cancel()
		select {
		case err := <-done:
			if err != nil {
				t.Fatalf("Run returned %v, want a clean shutdown", err)
			}
		case <-time.After(10 * time.Second):
			t.Fatal("Run did not return after the context was cancelled")
		}
	}
}

func TestRunServesClearH1AndShutsDownCleanly(t *testing.T) {
	t.Parallel()
	sockets := newTestListenerSockets(t)
	addr := sockets.reserveTCP()
	cfg := config.Default()
	cfg.Native.H1 = addr

	stop := runUntilCancel(t, &cfg, sockets)
	defer stop()

	res, err := http.Get("http://" + addr + "/")
	if err != nil {
		t.Fatalf("GET /: %v", err)
	}
	defer res.Body.Close()
	if csp := res.Header.Get("Content-Security-Policy"); !strings.HasPrefix(csp, "default-src 'self'; ") ||
		!strings.Contains(csp, "frame-ancestors 'none'") || !strings.Contains(csp, "connect-src 'self'") ||
		res.Header.Get("X-Frame-Options") != "DENY" || res.Header.Get("X-Content-Type-Options") != "nosniff" ||
		res.Header.Get("Referrer-Policy") != "same-origin" {
		t.Fatalf("public page lacks its hardening headers: %v", res.Header)
	}
}

func TestRunClosesOpenedListenersOnBindFailure(t *testing.T) {
	sockets := newTestListenerSockets(t)
	cfg := config.Default()
	cfg.Native.H1 = sockets.reserveTCP() // opens first, then must be closed
	cfg.Native.H1TLS = "127.0.0.1:1"     // unreserved, so its bind fails
	cfg.TLSCert, cfg.TLSKey = runTestTLS(t)

	if err := runWithSockets(t.Context(), &cfg, sockets); err == nil {
		t.Fatal("Run succeeded despite a listener that could not bind")
	}
	// The H1 listener bound before the failure, so its port must be free again.
	reclaimed, err := net.Listen("tcp", cfg.Native.H1)
	if err != nil {
		t.Fatalf("the first listener kept %s after the bind failure: %v", cfg.Native.H1, err)
	}
	reclaimed.Close()
}

func TestRunRejectsInvalidConfig(t *testing.T) {
	cfg := config.Default()
	cfg.MaxConnections = -1 // fails validateLimits
	err := runWithSockets(t.Context(), &cfg, newTestListenerSockets(t))
	if err == nil {
		t.Fatal("Run accepted an invalid configuration")
	}
	if errors.Is(err, context.Canceled) {
		t.Fatalf("Run failed for the wrong reason: %v", err)
	}
}
