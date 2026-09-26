package server

import (
	"context"
	"crypto/tls"
	"errors"
	"io"
	"net"
	"net/netip"
	"testing"
	"time"

	"github.com/quic-go/quic-go"
	"github.com/zR-JB/graphite-meter/go/internal/transport"
)

type testAddr string

func (a testAddr) Network() string { return "tcp" }
func (a testAddr) String() string  { return string(a) }

// scriptedConn is a net.Conn stub with a preset remote address.
type scriptedConn struct {
	net.Conn
	remote net.Addr
	closed bool
}

func (c *scriptedConn) RemoteAddr() net.Addr { return c.remote }
func (c *scriptedConn) Close() error         { c.closed = true; return nil }

// scriptedListener hands out a preset queue of conns, then io.EOF.
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
	releaseA, ok := a.acquire(testAddr("192.0.2.1:1"), false)
	if !ok {
		t.Fatal("first connection rejected")
	}
	if _, ok := a.acquire(testAddr("192.0.2.1:2"), false); ok {
		t.Fatal("per-client overflow admitted")
	}
	releaseB, ok := a.acquire(testAddr("192.0.2.2:1"), false)
	if !ok {
		t.Fatal("second client rejected")
	}
	if _, ok := a.acquire(testAddr("192.0.2.3:1"), false); ok {
		t.Fatal("global overflow admitted")
	}
	stats := a.stats()
	if stats.active != 2 || stats.peak != 2 || stats.rejectedGlobal != 1 || stats.rejectedClient != 1 {
		t.Fatalf("stats = %+v, want 2 active, 2 peak, 1 global and 1 client rejection", stats)
	}
	releaseA()
	releaseA()
	if release, ok := a.acquire(testAddr("192.0.2.1:3"), false); !ok {
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
	// clientMax 1: Accept closes the second conn from a client and returns the next admissible one in the same call.
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
	_ = first.Close()
	_ = second.Close()
	if got := a.stats().active; got != 0 {
		t.Fatalf("active connections after repeated close = %d, want 0", got)
	}

	if _, err := ln.Accept(); !errors.Is(err, io.EOF) {
		t.Fatalf("drained listener error = %v, want EOF", err)
	}
}

func TestConnContextAdmitsAndReleasesOnCancel(t *testing.T) {
	a := newConnectionAdmission(1, 1, nil)
	ctx, cancel := context.WithCancel(t.Context())
	if _, err := a.connContext(ctx, &quic.ClientInfo{RemoteAddr: testAddr("192.0.2.1:1")}); err != nil {
		t.Fatalf("first connContext: %v", err)
	}
	if _, err := a.connContext(t.Context(), &quic.ClientInfo{RemoteAddr: testAddr("192.0.2.2:1")}); err == nil {
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

// Under load a QUIC Initial holds a connection slot only once Retry has validated its source address.
func TestLoadedQUICAdmissionValidatesTheSourceFirst(t *testing.T) {
	_, cm := protocolTestTLS(t)
	for _, loaded := range []bool{false, true} {
		t.Run(map[bool]string{false: "idle", true: "loaded"}[loaded], func(t *testing.T) {
			a := newConnectionAdmission(4, 4, nil)
			if loaded {
				release, _ := a.acquire(testAddr("192.0.2.1:1"), false)
				defer release()
			}
			pc, err := net.ListenPacket("udp", "127.0.0.1:0")
			if err != nil {
				t.Fatal(err)
			}
			tr := a.quicTransport(pc)
			defer tr.Close()
			admit, verified := tr.ConnContext, make(chan bool, 1)
			tr.ConnContext = func(ctx context.Context, info *quic.ClientInfo) (context.Context, error) {
				verified <- info.AddrVerified
				return admit(ctx, info)
			}
			ln, err := tr.Listen(cm.tlsConfig("gm-test"), transport.NewQUICConfig())
			if err != nil {
				t.Fatal(err)
			}
			defer ln.Close()
			ctx, cancel := context.WithTimeout(t.Context(), 5*time.Second)
			defer cancel()
			conn, err := quic.DialAddr(ctx, pc.LocalAddr().String(),
				&tls.Config{InsecureSkipVerify: true, NextProtos: []string{"gm-test"}},
				transport.NewQUICConfig()) //nolint:gosec // test certificate
			if err != nil {
				t.Fatalf("dial: %v", err)
			}
			defer conn.CloseWithError(0, "")
			if got := <-verified; got != loaded {
				t.Fatalf("admitted with a validated source = %v, want %v", got, loaded)
			}
		})
	}
}
