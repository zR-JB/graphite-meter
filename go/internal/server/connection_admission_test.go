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
	"testing/synctest"
	"time"

	"github.com/quic-go/quic-go"
	"github.com/quic-go/quic-go/http3"
	"github.com/zR-JB/graphite-meter/go/internal/config"
	"github.com/zR-JB/graphite-meter/go/internal/testkit"
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
	if stats := a.stats(); !over.closed || stats.peak != 2 || stats.rejectedClient != 1 {
		t.Fatalf("refused connection closed = %t, stats %+v, want 2 peak and 1 client rejection", over.closed, stats)
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
	synctest.Test(t, func(t *testing.T) {
		a := newConnectionAdmission(1, 1, nil)
		ctx, cancel := context.WithCancel(t.Context())
		if _, err := a.connContext(ctx, &quic.ClientInfo{RemoteAddr: testAddr("192.0.2.1:1")}); err != nil {
			t.Fatalf("first connContext: %v", err)
		}
		if _, err := a.connContext(t.Context(), &quic.ClientInfo{RemoteAddr: testAddr("192.0.2.2:1")}); err == nil {
			t.Fatal("second connContext admitted past the global limit")
		}
		cancel()
		synctest.Wait()
		if stats := a.stats(); stats.active != 0 || stats.rejectedGlobal != 1 {
			t.Fatalf("stats after cancel %+v, want the slot released and 1 global rejection", stats)
		}
	})
}

// Under load, or once its client holds a QUIC share, an Initial holds a slot only after Retry validated its source.
func TestLoadedQUICAdmissionValidatesTheSourceFirst(t *testing.T) {
	_, cm := protocolTestTLS(t)
	for name, tc := range map[string]struct {
		held     []string
		quic     bool
		verified bool
	}{
		"idle":                   {},
		"loaded":                 {[]string{"192.0.2.1:1", "192.0.2.2:1"}, false, true},
		"client holds QUIC":      {[]string{"127.0.0.1:1"}, true, true},
		"client holds TCP alone": {[]string{"127.0.0.1:1"}, false, false},
	} {
		t.Run(name, func(t *testing.T) {
			a := newConnectionAdmission(8, 4, nil)
			for _, addr := range tc.held {
				release, _ := a.acquire(testAddr(addr), tc.quic)
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
			if got := <-verified; got != tc.verified {
				t.Fatalf("admitted with a validated source = %v, want %v", got, tc.verified)
			}
		})
	}
}

// Browsers open one QUIC connection per WebTransport session, so the default share fits a client's connections.
func TestDefaultSessionShareFitsAClientsQUICConnections(t *testing.T) {
	if sessions := config.Default().MaxSessionsPerClient; sessions > maxClientQUICConnections {
		t.Fatalf("%d sessions per client exceed its %d QUIC connections", sessions, maxClientQUICConnections)
	}
}

// A peer that stalls a control exchange, or idles between exchanges, gives its connection slot back within the
// control deadline on every native listener.
func TestStalledPeersReleaseTheirConnectionSlots(t *testing.T) {
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
	// Unbuffered pipes stall an answer until its deadline, then a TLS close offers close_notify for five seconds.
	const released = 10 * time.Second
	slots := func(t *testing.T, build *listenerBuild, want int, after time.Duration) {
		t.Helper()
		time.Sleep(after)
		synctest.Wait()
		if got := build.connections.stats().active; got != want {
			t.Fatalf("%d connection slots held after %v, want %d", got, after, want)
		}
	}
	shape := func(e *endpoints) { e.controlTimeout = timeout }
	for _, alpn := range []string{"", "http/1.1"} {
		for stall, hold := range stalls {
			t.Run(alpn+" "+stall, func(t *testing.T) {
				synctest.Test(t, func(t *testing.T) {
					cfg := config.Default()
					cfg.Native.H1TLS = ":7247"
					build, sockets := pipeServer(t, &cfg, shape)
					var conn net.Conn
					if alpn == "" {
						conn, _ = sockets[cfg.Native.H1].dial(t.Context())
					} else {
						conn = sockets[cfg.Native.H1TLS].dialTLS(t, alpn)
					}
					hold(t, conn)
					slots(t, build, 1, 0)
					slots(t, build, 0, released)
				})
			})
		}
	}
	t.Run("h2 idle", func(t *testing.T) {
		synctest.Test(t, func(t *testing.T) {
			cfg := config.Default()
			cfg.Native.H2 = ":7248"
			build, sockets := pipeServer(t, &cfg, shape)
			protocols := &http.Protocols{}
			protocols.SetHTTP2(true)
			tr := &http.Transport{Protocols: protocols, DialTLSContext: func(context.Context, string,
				string) (net.Conn, error) {
				return sockets[cfg.Native.H2].dialTLS(t, "h2"), nil
			}}
			defer tr.CloseIdleConnections()
			res, err := (&http.Client{Transport: tr}).Get("https://meter/probe")
			if err != nil {
				t.Fatal(err)
			}
			_, _ = io.Copy(io.Discard, res.Body)
			res.Body.Close()
			slots(t, build, 1, 0)
			slots(t, build, 0, released)
		})
	})
	t.Run("h3 idle", func(t *testing.T) {
		cfg, build := startListeners(t, func(cfg *config.Config, sockets *testListenerSockets) {
			cfg.Native.H1, cfg.Native.H3 = sockets.reserveTCP(), sockets.reserveH3()
		}, shape)
		tr := &http3.Transport{QUICConfig: transport.NewQUICConfig(),
			TLSClientConfig: &tls.Config{InsecureSkipVerify: true}} //nolint:gosec // test certificate
		defer tr.Close()
		res, err := (&http.Client{Transport: tr}).Get("https://" + cfg.Native.H3 + "/probe")
		if err != nil {
			t.Fatal(err)
		}
		res.Body.Close()
		testkit.Eventually(t, released, "an idle HTTP/3 connection gives its slot back",
			func() bool { return build.connections.stats().active == 0 })
	})
	t.Run("h3 stalled headers", func(t *testing.T) {
		cfg, build := startListeners(t, func(cfg *config.Config, sockets *testListenerSockets) {
			cfg.Native.H1, cfg.Native.H3 = sockets.reserveTCP(), sockets.reserveH3()
		}, shape)
		quicConfig := transport.NewQUICConfig()
		quicConfig.KeepAlivePeriod = timeout / 4
		conn, err := quic.DialAddr(t.Context(), cfg.Native.H3,
			&tls.Config{InsecureSkipVerify: true, NextProtos: []string{http3.NextProtoH3}}, //nolint:gosec // test certificate
			quicConfig)
		if err != nil {
			t.Fatal(err)
		}
		defer conn.CloseWithError(0, "")
		str, err := conn.OpenStream()
		if err != nil {
			t.Fatal(err)
		}
		if _, err := str.Write([]byte{0x01}); err != nil {
			t.Fatal(err)
		}
		select {
		case <-conn.Context().Done():
		case <-time.After(released):
			t.Fatal("a request stream stalled before its headers held its connection")
		}
		testkit.Eventually(t, released, "a closed HTTP/3 connection gives its slot back",
			func() bool { return build.connections.stats().active == 0 })
	})
}
