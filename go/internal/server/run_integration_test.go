package server

import (
	"context"
	"crypto/tls"
	"errors"
	"io"
	"maps"
	"net"
	"net/http"
	"testing"
	"time"

	"github.com/zR-JB/graphite-meter/go/internal/config"
)

// testListenerSockets reserves the exact sockets an integration test will hand
// to listener assembly. In particular H3 reserves TCP and UDP on the same port
// at the same time, removing the release/rebind TOCTOU that made race/shuffle
// runs intermittently fail with "address already in use".
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

func (s *testListenerSockets) listenTCP(addr string) (net.Listener, error) {
	if ln, ok := s.tcp[addr]; ok {
		delete(s.tcp, addr)
		return ln, nil
	}
	return net.Listen("tcp", addr)
}

func (s *testListenerSockets) listenUDP(addr string) (net.PacketConn, error) {
	if pc, ok := s.udp[addr]; ok {
		delete(s.udp, addr)
		return pc, nil
	}
	return net.ListenPacket("udp", addr)
}

// waitForOK polls a URL until it answers 200 or the deadline passes, so the
// test does not race the listeners coming up inside Run.
func waitForOK(t *testing.T, client *http.Client, url string) {
	t.Helper()
	deadline := time.Now().Add(10 * time.Second)
	for time.Now().Before(deadline) {
		res, err := client.Get(url)
		if err == nil {
			_, _ = io.Copy(io.Discard, res.Body)
			res.Body.Close()
			if res.StatusCode == http.StatusOK {
				return
			}
		}
		time.Sleep(20 * time.Millisecond)
	}
	t.Fatalf("server never served 200 at %s", url)
}

// runUntilCancel starts Run in the background and returns a stop function that
// cancels it and asserts a clean (nil) shutdown.
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

// TestRunServesClearH1AndShutsDownCleanly drives the whole Run lifecycle over a
// real loopback socket: validation, endpoint build, the clear-H1 assemble path,
// runServices, and a clean shutdown on cancel.
func TestRunServesClearH1AndShutsDownCleanly(t *testing.T) {
	sockets := newTestListenerSockets(t)
	addr := sockets.reserveTCP()
	cfg := config.Default()
	cfg.Native.H1 = addr

	stop := runUntilCancel(t, &cfg, sockets)
	defer stop()

	base := "http://" + addr
	waitForOK(t, http.DefaultClient, base+"/preflight")

	res, err := http.Get(base + "/preflight")
	if err != nil {
		t.Fatalf("GET /preflight: %v", err)
	}
	defer res.Body.Close()
	if ct := res.Header.Get("Content-Type"); ct == "" {
		t.Fatal("preflight response carried no content type")
	}
}

// TestRunServesTLSH1 additionally exercises the TLS listener build: tcpTLS, the
// certificate manager, and the HTTPS assemble branch.
func TestRunServesTLSH1(t *testing.T) {
	cert, key := writeCertificate(t, t.TempDir(), "srv", "127.0.0.1",
		time.Now().Add(-time.Hour), time.Now().Add(time.Hour))
	sockets := newTestListenerSockets(t)
	cfg := config.Default()
	cfg.Native.H1 = sockets.reserveTCP()
	tlsAddr := sockets.reserveTCP()
	cfg.Native.H1TLS = tlsAddr
	cfg.TLSCert, cfg.TLSKey = cert, key

	stop := runUntilCancel(t, &cfg, sockets)
	defer stop()

	client := &http.Client{Transport: &http.Transport{
		TLSClientConfig: &tls.Config{InsecureSkipVerify: true},
	}}
	waitForOK(t, client, "https://"+tlsAddr+"/preflight")
}

// TestRunServesH3 exercises assembleH3: the QUIC UDP listener plus its TCP
// Alt-Svc bootstrap companion. It proves only that the sockets bind and shut
// down cleanly: an H3 client round-trip is out of scope here.
func TestRunServesH3(t *testing.T) {
	cert, key := writeCertificate(t, t.TempDir(), "srv", "127.0.0.1",
		time.Now().Add(-time.Hour), time.Now().Add(time.Hour))
	sockets := newTestListenerSockets(t)
	cfg := config.Default()
	cfg.Native.H1 = sockets.reserveTCP()
	h3Addr := sockets.reserveH3() // same reserved port for TCP bootstrap and UDP
	cfg.Native.H3 = h3Addr
	cfg.TLSCert, cfg.TLSKey = cert, key

	stop := runUntilCancel(t, &cfg, sockets)
	defer stop()

	// The bootstrap companion is a TCP listener on the H3 address; a successful
	// dial proves assembleH3 bound its sockets.
	deadline := time.Now().Add(10 * time.Second)
	for {
		conn, err := net.DialTimeout("tcp", h3Addr, 200*time.Millisecond)
		if err == nil {
			_ = conn.Close()
			break
		}
		if time.Now().After(deadline) {
			t.Fatal("H3 bootstrap listener never came up")
		}
		time.Sleep(20 * time.Millisecond)
	}
}

// TestRunClosesOpenedListenersOnBindFailure forces the second listener's bind to
// fail so Run unwinds through closeOpened and returns the error.
func TestRunClosesOpenedListenersOnBindFailure(t *testing.T) {
	cert, key := writeCertificate(t, t.TempDir(), "srv", "127.0.0.1",
		time.Now().Add(-time.Hour), time.Now().Add(time.Hour))

	// Hold a port so the TLS listener cannot bind it.
	occupied, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	defer occupied.Close()

	sockets := newTestListenerSockets(t)
	cfg := config.Default()
	cfg.Native.H1 = sockets.reserveTCP()        // opens first, then must be closed
	cfg.Native.H1TLS = occupied.Addr().String() // bind fails here
	cfg.TLSCert, cfg.TLSKey = cert, key

	if err = runWithSockets(t.Context(), &cfg, sockets); err == nil {
		t.Fatal("Run succeeded despite a listener that could not bind")
	}
	// The H1 listener bound before the failure, so its port must be free again.
	reclaimed, err := net.Listen("tcp", cfg.Native.H1)
	if err != nil {
		t.Fatalf("the first listener kept %s after the bind failure: %v", cfg.Native.H1, err)
	}
	reclaimed.Close()
}

// TestRunRejectsInvalidConfig proves Run rejects a bad config without binding.
func TestRunRejectsInvalidConfig(t *testing.T) {
	cfg := config.Default()
	cfg.MaxConnections = -1 // fails validateLimits
	err := Run(t.Context(), &cfg)
	if err == nil {
		t.Fatal("Run accepted an invalid configuration")
	}
	if errors.Is(err, context.Canceled) {
		t.Fatalf("Run failed for the wrong reason: %v", err)
	}
}
