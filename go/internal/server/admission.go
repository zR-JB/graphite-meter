package server

import (
	"context"
	"errors"
	"net"
	"net/http"
	"net/netip"
	"sync"
	"time"

	"github.com/quic-go/quic-go"
	"github.com/zR-JB/graphite-meter/go/internal/auth"
	"github.com/zR-JB/graphite-meter/go/internal/endpoint"
	"github.com/zR-JB/graphite-meter/go/internal/route"
	"github.com/zR-JB/graphite-meter/go/internal/transport"
)

// budget counts concurrent occupancy against a global and a per-client ceiling; its owner locks it.
type budget struct {
	active, peak, limit, clientLimit int
	clients                          map[string]int
	rejectedGlobal, rejectedClient   uint64
}

func newBudget(limit, clientLimit int) budget {
	return budget{limit: limit, clientLimit: clientLimit, clients: make(map[string]int)}
}

func (b *budget) clientFull(key string) bool {
	if key != "" && b.clients[key] >= b.clientLimit {
		b.rejectedClient++
		return true
	}
	return false
}

func (b *budget) full() bool {
	if b.active >= b.limit {
		b.rejectedGlobal++
		return true
	}
	return false
}

func (b *budget) take(key string) {
	b.active++
	b.peak = max(b.peak, b.active)
	if key != "" {
		b.clients[key]++
	}
}

func (b *budget) give(key string) {
	b.active--
	if key != "" {
		if b.clients[key]--; b.clients[key] == 0 {
			delete(b.clients, key)
		}
	}
}

func (b *budget) snapshot() budget {
	c := *b
	c.clients = nil
	return c
}

// requestAdmission bounds measurement handlers; sessions also spend a per-login share of the pool.
type requestAdmission struct {
	mu                               sync.Mutex
	requests, sessions               budget
	requestLifetime, sessionLifetime time.Duration
}

func newRequestAdmission(globalMax, clientMax, sessionMax, sessionClientMax int, requestLifetime,
	sessionLifetime time.Duration) *requestAdmission {
	return &requestAdmission{
		requests: newBudget(globalMax, clientMax), sessions: newBudget(sessionMax, sessionClientMax),
		requestLifetime: requestLifetime, sessionLifetime: sessionLifetime,
	}
}

func (a *requestAdmission) acquire(key, sessionKey string) (release func(), status int) {
	a.mu.Lock()
	defer a.mu.Unlock()
	if sessionKey == "" {
		if a.requests.clientFull(key) {
			return nil, http.StatusTooManyRequests
		}
		if a.requests.full() {
			return nil, http.StatusServiceUnavailable
		}
		a.requests.take(key)
		return func() { a.mu.Lock(); a.requests.give(key); a.mu.Unlock() }, 0
	}
	if a.sessions.clientFull(sessionKey) {
		return nil, http.StatusTooManyRequests
	}
	if a.requests.full() || a.sessions.full() {
		return nil, http.StatusServiceUnavailable
	}
	a.requests.take("")
	a.sessions.take(sessionKey)
	return func() { a.mu.Lock(); a.requests.give(""); a.sessions.give(sessionKey); a.mu.Unlock() }, 0
}

func (a *requestAdmission) load() (active, max int) {
	a.mu.Lock()
	defer a.mu.Unlock()
	return a.requests.active, a.requests.limit
}

func (a *requestAdmission) stats() (requests, sessions budget) {
	a.mu.Lock()
	defer a.mu.Unlock()
	return a.requests.snapshot(), a.sessions.snapshot()
}

// wrap admits requests under spec's budget and lifetime; refusals carry the route's CORS answer.
func (a *requestAdmission) wrap(next http.Handler, spec route.Spec, trusted []netip.Prefix,
	authn *auth.Service) http.Handler {
	session := spec.Admission == route.Session
	lifetime := a.requestLifetime
	if session {
		lifetime = a.sessionLifetime
	}
	// Held channels take no socket deadline.
	request := spec.Kind == route.HTTP
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		key, sessionKey := endpoint.ClientKey(r, trusted), ""
		if session {
			sessionKey = endpoint.SessionKey(r, key)
		}
		release, status := a.acquire(key, sessionKey)
		if status != 0 {
			authn.MeasurementCORS(w.Header(), r)
			w.Header().Set("Retry-After", "1")
			http.Error(w, http.StatusText(status), status)
			return
		}
		defer release()
		ctx, cancel := context.WithTimeout(r.Context(), lifetime)
		defer cancel()
		if request {
			deadline, _ := ctx.Deadline()
			defer setSocketDeadlines(w, deadline)()
		}
		next.ServeHTTP(w, r.WithContext(ctx))
	})
}

func setSocketDeadlines(w http.ResponseWriter, deadline time.Time) func() {
	controller := http.NewResponseController(w)
	_ = controller.SetReadDeadline(deadline)
	_ = controller.SetWriteDeadline(deadline)
	return func() {
		_ = controller.SetReadDeadline(time.Time{})
		_ = controller.SetWriteDeadline(time.Time{})
	}
}

// connectionAdmission bounds TCP and QUIC connections per direct client; trusted proxies are exempt.
type connectionAdmission struct {
	mu          sync.Mutex
	connections budget
	trusted     []netip.Prefix
}

func newConnectionAdmission(globalMax, clientMax int, trusted []netip.Prefix) *connectionAdmission {
	return &connectionAdmission{connections: newBudget(globalMax, clientMax), trusted: trusted}
}

// socketKey buckets a direct peer; the empty key exempts a trusted proxy.
func socketKey(addr net.Addr, trusted []netip.Prefix) string {
	var ip netip.Addr
	switch a := addr.(type) {
	case *net.TCPAddr:
		ip = a.AddrPort().Addr()
	case *net.UDPAddr:
		ip = a.AddrPort().Addr()
	default:
		addrPort, err := netip.ParseAddrPort(addr.String())
		if err != nil {
			return "unknown"
		}
		ip = addrPort.Addr()
	}
	if transport.Trusted(ip, trusted) {
		return ""
	}
	return transport.AddressBucket(ip)
}

func (a *connectionAdmission) acquire(addr net.Addr) (func(), bool) {
	key := socketKey(addr, a.trusted)
	a.mu.Lock()
	defer a.mu.Unlock()
	if a.connections.clientFull(key) || a.connections.full() {
		return nil, false
	}
	a.connections.take(key)
	return sync.OnceFunc(func() {
		a.mu.Lock()
		a.connections.give(key)
		a.mu.Unlock()
	}), true
}

func (a *connectionAdmission) stats() budget {
	a.mu.Lock()
	defer a.mu.Unlock()
	return a.connections.snapshot()
}

// verifySourceAddress requires Retry under load, so spoofed Initials cannot hold slots.
func (a *connectionAdmission) verifySourceAddress(net.Addr) bool {
	a.mu.Lock()
	defer a.mu.Unlock()
	return a.connections.active >= a.connections.limit/4
}

func (a *connectionAdmission) connContext(ctx context.Context, info *quic.ClientInfo) (context.Context, error) {
	release, ok := a.acquire(info.RemoteAddr)
	if !ok {
		return nil, errors.New("connection capacity exhausted")
	}
	context.AfterFunc(ctx, release)
	return ctx, nil
}

func (a *connectionAdmission) quicTransport(pc net.PacketConn) *quic.Transport {
	return &quic.Transport{Conn: pc, ConnContext: a.connContext, VerifySourceAddress: a.verifySourceAddress}
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
}

func (c *admittedConn) Close() error {
	err := c.Conn.Close()
	c.release()
	return err
}
