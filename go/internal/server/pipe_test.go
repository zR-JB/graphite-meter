package server

import (
	"context"
	"crypto/tls"
	"errors"
	"fmt"
	"net"
	"net/http"
	"sync"
	"testing"
	"time"

	"github.com/zR-JB/graphite-meter/go/internal/config"
)

// pipeListener serves in-memory connections, so a synctest bubble can run real exchanges on fake time.
type pipeListener struct {
	conns     chan net.Conn
	done      chan struct{}
	closeOnce sync.Once
}

type pipeConn struct{ net.Conn }

func (pipeConn) RemoteAddr() net.Addr { return &net.TCPAddr{IP: net.IPv4(192, 0, 2, 1), Port: 1} }

func newPipeListener() *pipeListener {
	return &pipeListener{conns: make(chan net.Conn), done: make(chan struct{})}
}

func (l *pipeListener) Accept() (net.Conn, error) {
	select {
	case c := <-l.conns:
		return c, nil
	case <-l.done:
		return nil, net.ErrClosed
	}
}

func (l *pipeListener) Close() error {
	l.closeOnce.Do(func() { close(l.done) })
	return nil
}

func (l *pipeListener) Addr() net.Addr { return pipeConn{}.RemoteAddr() }

func (l *pipeListener) dial(ctx context.Context) (net.Conn, error) {
	client, server := net.Pipe()
	select {
	case l.conns <- pipeConn{server}:
		return client, nil
	case <-ctx.Done():
		return nil, ctx.Err()
	}
}

func (l *pipeListener) dialTLS(t *testing.T, alpn string) net.Conn {
	t.Helper()
	raw, err := l.dial(t.Context())
	if err != nil {
		t.Fatal(err)
	}
	conn := tls.Client(raw, &tls.Config{InsecureSkipVerify: true, NextProtos: []string{alpn}}) //nolint:gosec
	if err := conn.HandshakeContext(t.Context()); err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = conn.Close() })
	return conn
}

func (l *pipeListener) client(t *testing.T) *http.Client {
	tr := &http.Transport{DialContext: func(ctx context.Context, _, _ string) (net.Conn, error) { return l.dial(ctx) }}
	t.Cleanup(tr.CloseIdleConnections)
	return &http.Client{Transport: tr}
}

type pipeSockets map[string]*pipeListener

func (s pipeSockets) listenTCP(addr string) (net.Listener, error) {
	if ln, ok := s[addr]; ok {
		return ln, nil
	}
	return nil, fmt.Errorf("no pipe listener for %s", addr)
}

func (pipeSockets) listenUDP(string) (net.PacketConn, error) {
	return nil, errors.New("no UDP in a bubble")
}

func pipeServer(t *testing.T, cfg *config.Config, shape func(*endpoints)) (*listenerBuild, pipeSockets) {
	t.Helper()
	cfg.TLSCert, cfg.TLSKey = writeCertificate(t, t.TempDir(), "srv", "127.0.0.1",
		time.Now().Add(-time.Hour), time.Now().Add(time.Hour))
	sockets := pipeSockets{}
	for _, addr := range []string{cfg.Native.H1, cfg.Native.H1TLS, cfg.Native.H2} {
		if addr != "" {
			sockets[addr] = newPipeListener()
		}
	}
	ctx, cancel := context.WithCancel(t.Context())
	build, err := newListenerBuild(ctx, cfg, sockets)
	if err != nil {
		t.Fatal(err)
	}
	if shape != nil {
		shape(build.e)
	}
	if err := build.assemble(); err != nil {
		t.Fatal(err)
	}
	for _, svc := range build.services {
		go func() { _ = svc.run() }()
		t.Cleanup(func() { _ = svc.stop(context.Background()) })
	}
	t.Cleanup(cancel)
	return build, sockets
}
