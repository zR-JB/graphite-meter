package server

import (
	"context"
	"errors"
	"io"
	"net"
	"net/http"
	"net/netip"
	"sync"
	"time"

	"github.com/quic-go/quic-go"
	"github.com/zR-JB/graphite-meter/go/internal/auth"
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

func (b *budget) clientFull(keys ...string) bool {
	full := transport.ShareFull(keys, b.clientLimit, func(key string) int { return b.clients[key] })
	if full {
		b.rejectedClient++
	}
	return full
}

func (b *budget) full() bool {
	if b.active >= b.limit {
		b.rejectedGlobal++
		return true
	}
	return false
}

func (b *budget) take(keys ...string) {
	b.active++
	b.peak = max(b.peak, b.active)
	for _, key := range keys {
		b.clients[key]++
	}
}

func (b *budget) give(keys ...string) {
	b.active--
	for _, key := range keys {
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

func (a *requestAdmission) acquire(session bool, keys ...string) (release func(), status int) {
	a.mu.Lock()
	defer a.mu.Unlock()
	if !session {
		if a.requests.clientFull(keys...) {
			return nil, http.StatusTooManyRequests
		}
		if a.requests.full() {
			return nil, http.StatusServiceUnavailable
		}
		a.requests.take(keys...)
		return func() { a.mu.Lock(); a.requests.give(keys...); a.mu.Unlock() }, 0
	}
	if a.sessions.clientFull(keys...) {
		return nil, http.StatusTooManyRequests
	}
	if a.requests.full() || a.sessions.full() {
		return nil, http.StatusServiceUnavailable
	}
	a.requests.take()
	a.sessions.take(keys...)
	return func() { a.mu.Lock(); a.requests.give(); a.sessions.give(keys...); a.mu.Unlock() }, 0
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
	request := spec.Kind == route.HTTP
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		keys, ok := auth.ClientKeys(r, trusted)
		if !ok {
			authn.MeasurementCORS(w.Header(), r)
			http.Error(w, "ambiguous client address", http.StatusBadRequest)
			return
		}
		release, status := a.acquire(session, keys...)
		if status != 0 {
			authn.MeasurementCORS(w.Header(), r)
			w.Header().Set("Retry-After", "1")
			http.Error(w, http.StatusText(status), status)
			return
		}
		defer release()
		ctx, cancel := context.WithTimeout(r.Context(), lifetime)
		defer cancel()
		// A request is bounded by its lifetime; a held channel by its own idle policy, not the socket.
		var deadline time.Time
		if request {
			deadline, _ = ctx.Deadline()
		}
		controller := http.NewResponseController(w)
		_ = controller.SetReadDeadline(deadline)
		_ = controller.SetWriteDeadline(deadline)
		next.ServeHTTP(w, r.WithContext(ctx))
	})
}

// boundedRequest refuses non-POST bodies and bounds each exchange; admission extends measurements.
func boundedRequest(next http.Handler, timeout time.Duration) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.Method != http.MethodPost && (r.ContentLength > 0 || r.ContentLength < 0 && r.ProtoMajor < 3) {
			// Closing the connection skips the drain an HTTP/1 answer would otherwise start with.
			if r.ProtoMajor == 1 {
				w.Header().Set("Connection", "close")
			}
			http.Error(w, "request body not accepted", http.StatusBadRequest)
			return
		}
		controller := http.NewResponseController(w)
		_ = controller.SetReadDeadline(time.Now().Add(timeout))
		_ = controller.SetWriteDeadline(time.Now().Add(timeout))
		next.ServeHTTP(w, r)
		_ = controller.SetReadDeadline(time.Now().Add(timeout / 2))
		_ = controller.SetWriteDeadline(time.Now().Add(timeout))
	})
}

// connectionAdmission bounds TCP and QUIC connections per direct client; trusted proxies are exempt.
type connectionAdmission struct {
	mu                sync.Mutex
	connections, quic budget
	trusted           []netip.Prefix
}

func newConnectionAdmission(globalMax, clientMax int, trusted []netip.Prefix) *connectionAdmission {
	return &connectionAdmission{connections: newBudget(globalMax, clientMax),
		quic: newBudget(globalMax, min(clientMax, maxClientQUICConnections)), trusted: trusted}
}

func socketKeys(addr net.Addr, trusted []netip.Prefix) []string {
	ip, ok := transport.Peer(addr.String())
	if !ok {
		return []string{"unknown"}
	}
	if transport.Trusted(ip, trusted) {
		return nil
	}
	return transport.AddressKeys(ip)
}

func (a *connectionAdmission) acquire(addr net.Addr, quic bool) (func(), bool) {
	keys := socketKeys(addr, a.trusted)
	a.mu.Lock()
	defer a.mu.Unlock()
	if a.connections.clientFull(keys...) || a.connections.full() || quic && a.quic.clientFull(keys...) {
		return nil, false
	}
	a.connections.take(keys...)
	if quic {
		a.quic.take(keys...)
	}
	return sync.OnceFunc(func() {
		a.mu.Lock()
		defer a.mu.Unlock()
		a.connections.give(keys...)
		if quic {
			a.quic.give(keys...)
		}
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
	release, ok := a.acquire(info.RemoteAddr, true)
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
		release, ok := l.admission.acquire(conn.RemoteAddr(), false)
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

// CloseWrite lets the HTTP/1 server send its FIN before an early refusal's lingering close.
func (c *admittedConn) CloseWrite() error {
	if tcp, ok := c.Conn.(*net.TCPConn); ok {
		return tcp.CloseWrite()
	}
	return nil
}

// ReadFrom keeps the kernel's zero-copy path for bodies copied onto the connection.
func (c *admittedConn) ReadFrom(r io.Reader) (int64, error) { return io.Copy(c.Conn, r) }
