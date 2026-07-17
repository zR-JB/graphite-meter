package server

import (
	"context"
	"fmt"
	"net"
	"net/netip"
	"sync"

	"github.com/quic-go/quic-go"
)

type connectionAdmission struct {
	mu             sync.Mutex
	active         int
	byClient       map[string]int
	globalMax      int
	clientMax      int
	trusted        []netip.Prefix
	peak           int
	rejectedGlobal uint64
	rejectedClient uint64
}

func newConnectionAdmission(globalMax, clientMax int, trusted []netip.Prefix) *connectionAdmission {
	return &connectionAdmission{byClient: make(map[string]int), globalMax: globalMax, clientMax: clientMax, trusted: trusted}
}

func socketKey(addr net.Addr, trusted []netip.Prefix) string {
	addrPort, err := netip.ParseAddrPort(addr.String())
	if err != nil {
		return "unknown"
	}
	ip := addrPort.Addr().Unmap()
	for _, prefix := range trusted {
		if prefix.Contains(ip) {
			return ""
		}
	}
	if ip.Is6() {
		return netip.PrefixFrom(ip, 64).Masked().String()
	}
	return ip.String()
}

func (a *connectionAdmission) acquire(addr net.Addr) (func(), bool) {
	key := socketKey(addr, a.trusted)
	a.mu.Lock()
	defer a.mu.Unlock()
	if a.active >= a.globalMax {
		a.rejectedGlobal++
		return nil, false
	}
	if key != "" && a.byClient[key] >= a.clientMax {
		a.rejectedClient++
		return nil, false
	}
	a.active++
	if a.active > a.peak {
		a.peak = a.active
	}
	if key != "" {
		a.byClient[key]++
	}
	var once sync.Once
	return func() {
		once.Do(func() {
			a.mu.Lock()
			a.active--
			if key != "" {
				a.byClient[key]--
				if a.byClient[key] == 0 {
					delete(a.byClient, key)
				}
			}
			a.mu.Unlock()
		})
	}, true
}

func (a *connectionAdmission) stats() admissionStats {
	a.mu.Lock()
	defer a.mu.Unlock()
	return admissionStats{a.active, a.peak, a.rejectedGlobal, a.rejectedClient}
}

func (a *connectionAdmission) connContext(ctx context.Context, info *quic.ClientInfo) (context.Context, error) {
	release, ok := a.acquire(info.RemoteAddr)
	if !ok {
		return nil, fmt.Errorf("connection capacity exhausted")
	}
	context.AfterFunc(ctx, release)
	return ctx, nil
}

type admittedListener struct {
	net.Listener
	admission *connectionAdmission
}

func (l admittedListener) Accept() (net.Conn, error) {
	for {
		conn, err := l.Listener.Accept()
		if err != nil {
			return nil, err
		}
		release, ok := l.admission.acquire(conn.RemoteAddr())
		if !ok {
			_ = conn.Close()
			continue
		}
		return &admittedConn{Conn: conn, release: release}, nil
	}
}

type admittedConn struct {
	net.Conn
	release func()
	once    sync.Once
}

func (c *admittedConn) Close() error {
	err := c.Conn.Close()
	c.once.Do(c.release)
	return err
}
