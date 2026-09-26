package server

import (
	"bufio"
	"context"
	"crypto/tls"
	"errors"
	"io"
	"net"
	"net/http"
	"net/netip"
	"strings"
	"testing"
	"time"

	"github.com/quic-go/quic-go"
	"github.com/quic-go/quic-go/http3"
	"github.com/zR-JB/graphite-meter/go/internal/config"
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

// An IPv6 client is bounded per /64, and its /56 and /48 hold only two and four clients' shares.
func TestConnectionAdmissionBucketsIPv6Hierarchically(t *testing.T) {
	a := newConnectionAdmission(100, 2, []netip.Prefix{netip.MustParsePrefix("10.0.0.0/8")})
	for i, tc := range []struct {
		addr string
		want bool
	}{
		{"[2001:db8:0:100::1]:1", true}, {"[2001:db8:0:100::2]:1", true}, {"[2001:db8:0:100::3]:1", false},
		{"[2001:db8:0:101::1]:1", true}, {"[2001:db8:0:101::2]:1", true}, {"[2001:db8:0:102::1]:1", false},
		{"[2001:db8:0:200::1]:1", true}, {"[2001:db8:0:200::2]:1", true},
		{"[2001:db8:0:201::1]:1", true}, {"[2001:db8:0:201::2]:1", true}, {"[2001:db8:0:300::1]:1", false},
		{"[2001:db8:1::1]:1", true}, {"10.0.0.2:443", true}, {"10.0.0.2:443", true}, {"10.0.0.2:443", true},
	} {
		if _, ok := a.acquire(testAddr(tc.addr), false); ok != tc.want {
			t.Fatalf("connection %d from %s admitted = %t", i, tc.addr, ok)
		}
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

// A peer that stalls a control exchange, or idles between exchanges, gives its connection slot back within the
// control deadline on every native listener.
func TestStalledPeersReleaseTheirConnectionSlots(t *testing.T) {
	t.Parallel()
	const timeout = 200 * time.Millisecond
	send := func(request string) func(*testing.T, net.Conn) {
		return func(t *testing.T, c net.Conn) {
			if _, err := io.WriteString(c, request); err != nil {
				t.Fatal(err)
			}
		}
	}
	stalls := map[string]func(*testing.T, net.Conn){
		"partial headers": send("GET /probe HTTP/1.1\r\nHost: meter\r\n"),
		"pending body":    send("POST /upload/session HTTP/1.1\r\nHost: meter\r\nContent-Length: 10\r\n\r\n"),
		"idle keep-alive": func(t *testing.T, c net.Conn) {
			send("GET /missing HTTP/1.1\r\nHost: meter\r\n\r\n")(t, c)
			res, err := http.ReadResponse(bufio.NewReader(c), nil)
			if err != nil {
				t.Fatal(err)
			}
			_, _ = io.Copy(io.Discard, res.Body)
			res.Body.Close()
		},
		// Pipelined answers fill the socket buffers until a finished handler's flush blocks.
		"unread responses": func(_ *testing.T, c net.Conn) {
			go func() {
				request := strings.Repeat("GET /missing HTTP/1.1\r\nHost: meter\r\n\r\n", 64)
				for {
					if _, err := io.WriteString(c, request); err != nil {
						return
					}
				}
			}()
		},
	}
	dialers := map[string]func(*testing.T, *config.Config) net.Conn{
		"h1": func(t *testing.T, cfg *config.Config) net.Conn { return dialTCP(t, cfg.Native.H1) },
		"h1 tls": func(t *testing.T, cfg *config.Config) net.Conn {
			return dialTLS(t, cfg.Native.H1TLS, "http/1.1")
		},
		"h3 companion": func(t *testing.T, cfg *config.Config) net.Conn { return dialTLS(t, cfg.Native.H3, "http/1.1") },
	}
	for listener, dial := range dialers {
		for stall, hold := range stalls {
			t.Run(listener+" "+stall, func(t *testing.T) {
				t.Parallel()
				cfg, build := slotServer(t, timeout)
				hold(t, dial(t, cfg))
				awaitSlots(t, build.connections, 1)
				awaitSlots(t, build.connections, 0)
			})
		}
	}
	t.Run("h2 idle", func(t *testing.T) {
		t.Parallel()
		cfg, build := slotServer(t, timeout)
		protocols := &http.Protocols{}
		protocols.SetHTTP2(true)
		tr := &http.Transport{Protocols: protocols,
			TLSClientConfig: &tls.Config{InsecureSkipVerify: true}} //nolint:gosec // test certificate
		defer tr.CloseIdleConnections()
		getOnce(t, &http.Client{Transport: tr}, "https://"+cfg.Native.H2+"/probe")
		awaitSlots(t, build.connections, 1)
		awaitSlots(t, build.connections, 0)
	})
	t.Run("h3 idle", func(t *testing.T) {
		t.Parallel()
		cfg, build := slotServer(t, timeout)
		tr := &http3.Transport{QUICConfig: transport.NewQUICConfig(),
			TLSClientConfig: &tls.Config{InsecureSkipVerify: true}} //nolint:gosec // test certificate
		defer tr.Close()
		getOnce(t, &http.Client{Transport: tr}, "https://"+cfg.Native.H3+"/probe")
		awaitSlots(t, build.connections, 1)
		awaitSlots(t, build.connections, 0)
	})
}

func slotServer(t *testing.T, timeout time.Duration) (*config.Config, *listenerBuild) {
	t.Helper()
	return startListeners(t, func(cfg *config.Config, sockets *testListenerSockets) {
		cfg.Native.H1, cfg.Native.H1TLS, cfg.Native.H2 = sockets.reserveTCP(), sockets.reserveTCP(), sockets.reserveTCP()
		cfg.Native.H3 = sockets.reserveH3()
	}, func(e *endpoints) { e.controlTimeout = timeout })
}

// dialTCP opens a client socket whose small receive buffer lets unread responses back up quickly.
func dialTCP(t *testing.T, addr string) net.Conn {
	t.Helper()
	conn, err := net.Dial("tcp", addr)
	if err != nil {
		t.Fatal(err)
	}
	_ = conn.(*net.TCPConn).SetReadBuffer(4096)
	t.Cleanup(func() { _ = conn.Close() })
	return conn
}

func dialTLS(t *testing.T, addr, alpn string) net.Conn {
	t.Helper()
	conn := tls.Client(dialTCP(t, addr), &tls.Config{InsecureSkipVerify: true, //nolint:gosec // test certificate
		NextProtos: []string{alpn}})
	if err := conn.HandshakeContext(t.Context()); err != nil {
		t.Fatal(err)
	}
	return conn
}

func getOnce(t *testing.T, client *http.Client, url string) {
	t.Helper()
	res, err := client.Get(url)
	if err != nil {
		t.Fatal(err)
	}
	_, _ = io.Copy(io.Discard, res.Body)
	res.Body.Close()
	if res.StatusCode != http.StatusOK {
		t.Fatalf("GET %s = %d", url, res.StatusCode)
	}
}

// awaitSlots polls, since a server-side close reaches the count only after the peer could observe it. A TLS close
// may first spend five seconds offering close_notify to a peer that stopped reading.
func awaitSlots(t *testing.T, connections *connectionAdmission, want int) {
	t.Helper()
	start := time.Now()
	for connections.stats().active != want {
		if time.Since(start) > 7*time.Second {
			t.Fatalf("%d connection slots held after %v, want %d", connections.stats().active, time.Since(start), want)
		}
		time.Sleep(10 * time.Millisecond)
	}
}
