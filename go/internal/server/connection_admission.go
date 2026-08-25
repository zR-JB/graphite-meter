package server

import (
	"context"
	"errors"
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

// exemptKey opts an address out of the per-client connection limit.
const exemptKey = ""

// socketKey buckets a socket address per client. IPv6 groups by /64 because a
// single subscriber routinely holds a whole prefix. A trusted proxy address
// maps to exemptKey: every proxied connection shares that one address, so a
// per-client count caps the whole deployment.
func socketKey(addr net.Addr, trusted []netip.Prefix) string {
	addrPort, err := netip.ParseAddrPort(addr.String())
	if err != nil {
		return "unknown"
	}
	ip := addrPort.Addr().Unmap()
	for _, prefix := range trusted {
		if prefix.Contains(ip) {
			return exemptKey
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
	if key != exemptKey && a.byClient[key] >= a.clientMax {
		a.rejectedClient++
		return nil, false
	}
	a.active++
	a.peak = max(a.peak, a.active)
	if key != exemptKey {
		a.byClient[key]++
	}
	return sync.OnceFunc(func() {
		a.mu.Lock()
		a.active--
		if key != exemptKey {
			a.byClient[key]--
			if a.byClient[key] == 0 {
				delete(a.byClient, key)
			}
		}
		a.mu.Unlock()
	}), true
}

func (a *connectionAdmission) stats() admissionStats {
	a.mu.Lock()
	defer a.mu.Unlock()
	return admissionStats{a.active, a.peak, a.rejectedGlobal, a.rejectedClient}
}

func (a *connectionAdmission) connContext(ctx context.Context, info *quic.ClientInfo) (context.Context, error) {
	release, ok := a.acquire(info.RemoteAddr)
	if !ok {
		return nil, errors.New("connection capacity exhausted")
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
			// A refused connection is dropped without a reply. A close failure
			// on an abandoned socket has no recovery.
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
