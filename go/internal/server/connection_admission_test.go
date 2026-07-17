package server

import (
	"net"
	"net/netip"
	"testing"
)

type testAddr string

func (a testAddr) Network() string { return "tcp" }
func (a testAddr) String() string  { return string(a) }

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
		t.Fatalf("stats = %+v", stats)
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
		t.Fatalf("trusted proxy key = %q", got)
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
		t.Fatalf("release called %d times", called)
	}
}
