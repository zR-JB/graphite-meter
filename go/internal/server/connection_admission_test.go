package server

import (
	"context"
	"io"
	"net"
	"net/http"
	"net/netip"
	"testing"
	"time"

	"github.com/quic-go/quic-go"
)

type testAddr string

func (a testAddr) Network() string { return "tcp" }
func (a testAddr) String() string  { return string(a) }

// scriptedConn is a net.Conn stub that reports a fixed remote address and
// records whether Accept closed it after an admission refusal.
type scriptedConn struct {
	net.Conn
	remote net.Addr
	closed bool
}

func (c *scriptedConn) RemoteAddr() net.Addr { return c.remote }
func (c *scriptedConn) Close() error         { c.closed = true; return nil }

// scriptedListener hands out a fixed queue of conns, then io.EOF.
type scriptedListener struct {
	conns []net.Conn
	i     int
}

func (l *scriptedListener) Accept() (net.Conn, error) {
	if l.i >= len(l.conns) {
		return nil, io.EOF
	}
	c := l.conns[l.i]
	l.i++
	return c, nil
}

func (l *scriptedListener) Close() error   { return nil }
func (l *scriptedListener) Addr() net.Addr { return testAddr("127.0.0.1:0") }

func TestConnectionAdmissionLimitsAndRelease(t *testing.T) {
	a := newConnectionAdmission(2, 1, nil)
	releaseA, ok := a.acquire(testAddr("192.0.2.1:1"))
	if !ok {
		t.Fatal("first connection rejected")
	}
	if _, ok := a.acquire(testAddr("192.0.2.1:2")); ok {
		t.Fatal("per-client overflow admitted")
	}
	releaseB, ok := a.acquire(testAddr("192.0.2.2:1"))
	if !ok {
		t.Fatal("second client rejected")
	}
	if _, ok := a.acquire(testAddr("192.0.2.3:1")); ok {
		t.Fatal("global overflow admitted")
	}
	stats := a.stats()
	if stats.active != 2 || stats.peak != 2 || stats.rejectedGlobal != 1 || stats.rejectedClient != 1 {
		t.Fatalf("stats = %+v, want 2 active, 2 peak, 1 global and 1 client rejection", stats)
	}
	releaseA()
	releaseA()
	if release, ok := a.acquire(testAddr("192.0.2.1:3")); !ok {
		t.Fatal("released capacity was not reusable")
	} else {
		release()
	}
	releaseB()
}

func TestSocketKeyIPv6AndTrustedProxy(t *testing.T) {
	a := socketKey(testAddr("[2001:db8:1::1]:1"), nil)
	b := socketKey(testAddr("[2001:db8:1::ffff]:2"), nil)
	if a != b {
		t.Fatalf("same /64 produced %q and %q", a, b)
	}
	trusted := []netip.Prefix{netip.MustParsePrefix("10.0.0.0/8")}
	if got := socketKey(testAddr("10.0.0.2:443"), trusted); got != "" {
		t.Fatalf("trusted proxy key = %q, want the empty exemption key", got)
	}
}

func TestAdmittedListenerSkipsRefusedConnections(t *testing.T) {
	// clientMax 1: the first conn from a client is admitted, a second from the
	// same client is closed and skipped, and Accept returns the next admissible
	// conn from a different client in a single call.
	a := newConnectionAdmission(5, 1, nil)
	over := &scriptedConn{remote: testAddr("192.0.2.1:2")}
	ln := admittedListener{
		Listener: &scriptedListener{conns: []net.Conn{
			&scriptedConn{remote: testAddr("192.0.2.1:1")},
			over,
			&scriptedConn{remote: testAddr("192.0.2.2:1")},
		}},
		admission: a,
	}

	first, err := ln.Accept()
	if err != nil {
		t.Fatalf("first accept: %v", err)
	}
	second, err := ln.Accept()
	if err != nil {
		t.Fatalf("second accept: %v", err)
	}
	if !over.closed {
		t.Fatal("the refused connection was not closed")
	}
	if got := second.RemoteAddr().String(); got != "192.0.2.2:1" {
		t.Fatalf("second admitted conn = %q, want the different client", got)
	}
	_ = first.Close()
	_ = second.Close()

	if _, err := ln.Accept(); err != io.EOF {
		t.Fatalf("drained listener error = %v, want EOF", err)
	}
}

func TestConnContextAdmitsAndReleasesOnCancel(t *testing.T) {
	a := newConnectionAdmission(1, 1, nil)
	ctx, cancel := context.WithCancel(context.Background())
	if _, err := a.connContext(ctx, &quic.ClientInfo{RemoteAddr: testAddr("192.0.2.1:1")}); err != nil {
		t.Fatalf("first connContext: %v", err)
	}
	if _, err := a.connContext(context.Background(), &quic.ClientInfo{RemoteAddr: testAddr("192.0.2.2:1")}); err == nil {
		t.Fatal("second connContext admitted past the global limit")
	}

	// Cancelling the first connection's context frees its slot asynchronously.
	cancel()
	deadline := time.Now().Add(2 * time.Second)
	for a.stats().active != 0 {
		if time.Now().After(deadline) {
			t.Fatal("cancelled connection never released its slot")
		}
		time.Sleep(time.Millisecond)
	}
}

func TestHardenAuthenticatedServer(t *testing.T) {
	off := &http.Server{}
	hardenAuthenticatedServer(off, false)
	if off.IdleTimeout != 0 || off.MaxHeaderBytes != 0 {
		t.Fatalf("auth-off server was hardened: idle=%v max=%d", off.IdleTimeout, off.MaxHeaderBytes)
	}
	on := &http.Server{}
	hardenAuthenticatedServer(on, true)
	if on.IdleTimeout != 60*time.Second || on.MaxHeaderBytes != 32<<10 {
		t.Fatalf("auth-on server not hardened: idle=%v max=%d", on.IdleTimeout, on.MaxHeaderBytes)
	}
}

func TestAdmittedConnReleasesOnce(t *testing.T) {
	server, client := net.Pipe()
	defer client.Close()
	called := 0
	conn := &admittedConn{Conn: server, release: func() { called++ }}
	_ = conn.Close()
	_ = conn.Close()
	if called != 1 {
		t.Fatalf("release called %d times, want 1", called)
	}
}
