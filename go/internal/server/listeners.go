// Package server builds one measurement core and mounts it on protocol-specific listeners.
package server

import (
	"cmp"
	"context"
	"crypto/rand"
	"crypto/tls"
	"errors"
	"fmt"
	"io"
	"log"
	"maps"
	"net"
	"net/http"
	"net/netip"
	"net/url"
	"slices"
	"strings"
	"sync"
	"time"

	"github.com/quic-go/quic-go"
	"github.com/quic-go/quic-go/http3"
	"github.com/quic-go/webtransport-go"
	"github.com/zR-JB/graphite-meter/go/internal/auth"
	"github.com/zR-JB/graphite-meter/go/internal/config"
	"github.com/zR-JB/graphite-meter/go/internal/endpoint"
	"github.com/zR-JB/graphite-meter/go/internal/logx"
	"github.com/zR-JB/graphite-meter/go/internal/static"
	"github.com/zR-JB/graphite-meter/go/internal/transport"
	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

const (
	downloadBlockSize            = 256 * 1024
	h3ControlStreams             = 4
	browserH3UniStreams          = 3
	wtLaneCreditHeadroom         = 4
	h2ReceiveWindowPerConnection = 16 << 20
	h2ReceiveWindowPerStream     = 8 << 20
	// Each open request stream may hold a HEADERS buffer this large before any deadline applies.
	h3MaxHeaderBytes = 4 << 10
	// A client's QUIC connections each carry that many request streams, so they have their own small share.
	maxClientQUICConnections = 8
	// controlTimeout bounds an idle connection, a handshake and every exchange outside measurement admission.
	controlTimeout = 15 * time.Second
)

type endpoints struct {
	discovery             *endpoint.Discovery
	probe, bootstrapProbe *endpoint.Probe
	download              *endpoint.Download
	upload                *endpoint.Upload
	admission             *requestAdmission
	trusted               []netip.Prefix
	idleBound             time.Duration
	controlTimeout        time.Duration
}

// A service's name is its protocol, which its errors lead with; its roles say what it serves.
type service struct {
	name, roles, addr, network string
	run                        func() error
	stop                       func(context.Context) error
}

type listenerSockets interface {
	listenTCP(string) (net.Listener, error)
	listenUDP(string) (net.PacketConn, error)
}

type systemListenerSockets struct{}

func (systemListenerSockets) listenTCP(addr string) (net.Listener, error) {
	return net.Listen("tcp", addr)
}

func (systemListenerSockets) listenUDP(addr string) (net.PacketConn, error) {
	return net.ListenPacket("udp", addr)
}

func buildEndpoints(ctx context.Context, cfg *config.Config) *endpoints {
	block := make([]byte, downloadBlockSize)
	_, _ = rand.Read(block) // crypto/rand.Read never fails
	var downloadMeter, uploadMeter *endpoint.Meter
	if cfg.Verbose {
		downloadMeter, uploadMeter = endpoint.NewMeter("server:download"), endpoint.NewMeter("server:upload")
		go downloadMeter.Run(ctx)
		go uploadMeter.Run(ctx)
	}

	admission := newRequestAdmission(cfg.MaxActiveMeasurements, cfg.MaxActiveMeasurementsPerClient,
		cfg.MaxActiveSessions, cfg.MaxSessionsPerClient, cfg.MaxOperationDuration, cfg.MaxSessionDuration)
	download, upload := endpoint.NewDownload(block, downloadMeter), endpoint.NewUpload(uploadMeter, cfg.TrustedProxies)
	go upload.RunSweeper(ctx)
	return &endpoints{
		discovery:      endpoint.NewDiscovery(cfg),
		probe:          endpoint.NewProbe(cfg.TrustedProxies, "", admission.load),
		bootstrapProbe: endpoint.NewProbe(cfg.TrustedProxies, publicH3Port(cfg), admission.load),
		download:       download,
		upload:         upload,
		admission:      admission,
		trusted:        cfg.TrustedProxies,
		idleBound:      wire.IdleBound,
		controlTimeout: controlTimeout,
	}
}

func publicH3Port(cfg *config.Config) string {
	if cfg.NativePublic.H3 != "" {
		u, err := url.Parse(cfg.NativePublic.H3)
		if err == nil {
			return cmp.Or(u.Port(), "443")
		}
	}
	_, port, _ := net.SplitHostPort(cfg.Native.H3)
	return port
}

func baseServer(handler http.Handler, protocols *http.Protocols, timeout time.Duration, peers *peerLog) *http.Server {
	return &http.Server{Handler: boundedRequest(handler, timeout), ReadTimeout: timeout, WriteTimeout: timeout,
		IdleTimeout: timeout, MaxHeaderBytes: 32 << 10, Protocols: protocols, ErrorLog: log.New(peers, "", 0),
		HTTP2: &http.HTTP2Config{
			// Bound upload DATA frames so control requests can share a saturated connection.
			MaxReadFrameSize: 16 << 10,
			// The 1 MiB defaults cap an upload at 1 MiB per RTT; buffers fill lazily.
			MaxReceiveBufferPerConnection: h2ReceiveWindowPerConnection,
			MaxReceiveBufferPerStream:     h2ReceiveWindowPerStream,
		}, ConnContext: func(ctx context.Context, c net.Conn) context.Context {
			if encrypted, ok := c.(*tls.Conn); ok {
				c = encrypted.NetConn()
			}
			if admitted, ok := c.(*admittedConn); ok {
				c = admitted.Conn
			}
			if tc, ok := c.(*net.TCPConn); ok {
				_ = tc.SetNoDelay(true)
				if protocols != nil && protocols.HTTP2() {
					configureHTTP2TCP(tc)
				}
			}
			return ctx
		}}
}

func Run(ctx context.Context, cfg *config.Config) error {
	return runWithSockets(ctx, cfg, systemListenerSockets{})
}

func runWithSockets(ctx context.Context, cfg *config.Config, sockets listenerSockets) error {
	// Background sweepers, pollers and loggers end with the services, whatever ended them.
	ctx, cancel := context.WithCancel(ctx)
	defer cancel()
	b, err := newListenerBuild(ctx, cfg, sockets)
	if err != nil {
		return err
	}
	if err := b.assemble(); err != nil {
		return err
	}
	return runServices(ctx, cfg, b.services)
}

func newListenerBuild(ctx context.Context, cfg *config.Config, sockets listenerSockets) (*listenerBuild, error) {
	if err := cfg.Validate(); err != nil {
		return nil, err
	}
	authn, err := auth.New(ctx, cfg.Auth, cfg.TrustedProxies, cfg.Verbose)
	if err != nil {
		return nil, err
	}
	var cm *certificateManager
	if cfg.TLSEnabled() {
		if cm, err = newCertificateManager(cfg); err != nil {
			return nil, err
		}
		go cm.run(ctx)
	}
	e := buildEndpoints(ctx, cfg)
	connections := newConnectionAdmission(cfg.MaxConnections, cfg.MaxConnectionsPerClient, cfg.TrustedProxies)
	if authn.Enabled() {
		authn.SetConnectOrigins(slices.Concat(e.discovery.ConnectOrigins(authn.PublicHostname()),
			cfg.ServerCatalog.ConnectSources()))
	}
	page := static.Handler(authn.Enabled(), cfg.ResultHistoryDefault)
	spa := http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		policy := authn.PagePolicy()
		if policy == "" {
			policy = e.discovery.PagePolicy(endpoint.RequestHost(r))
		}
		w.Header().Set("Content-Security-Policy", policy)
		w.Header().Set("X-Frame-Options", "DENY")
		auth.HardeningHeaders(w.Header())
		page.ServeHTTP(w, r)
	})
	if cfg.Verbose {
		go runAdmissionLog(ctx, e.admission, connections)
	}
	return &listenerBuild{ctx: ctx, cfg: cfg, e: e, authn: authn, cm: cm, connections: connections, spa: spa,
		sockets: sockets}, nil
}

type listenerBuild struct {
	ctx         context.Context
	cfg         *config.Config
	e           *endpoints
	authn       *auth.Service
	cm          *certificateManager
	connections *connectionAdmission
	spa         http.Handler
	peers       peerLog
	services    []service
	opened      []io.Closer
	sockets     listenerSockets
}

type tcpListener struct {
	name, roles, addr, alpn string
	listener                auth.Listener
	topo                    muxTopology
}

func (b *listenerBuild) assemble() (err error) {
	defer func() {
		if err != nil {
			for _, c := range b.opened {
				_ = c.Close()
			}
		}
	}()
	cfg := b.cfg
	ui := muxTopology{spa: true, discovery: true, latency: true, transfers: true}
	uiTLS := ui
	uiTLS.requiredProto = 1
	const all = "UI, discovery, probe, transfers, WebSockets"
	h1Roles := all
	if b.authn.Enabled() {
		h1Roles = "trusted proxy only; GET / redirects to HTTPS"
	}
	for _, l := range []tcpListener{
		{"HTTP/1.1", h1Roles, cfg.Native.H1, "", auth.Listener{UI: true}, ui},
		{"HTTPS/1.1", all, cfg.Native.H1TLS, "http/1.1", auth.Listener{UI: true}, uiTLS},
		{"HTTPS/2", "probe, transfers, progress", cfg.Native.H2, "h2",
			auth.Listener{}, muxTopology{transfers: true, requiredProto: 2}},
		{"HTTPS/1.1", "HTTP/3 bootstrap probe, upload, tickets", cfg.Native.H3, "http/1.1",
			auth.Listener{}, muxTopology{bootstrap: true, control: true}},
	} {
		if l.addr == "" {
			continue
		}
		if err := b.addTCP(l); err != nil {
			return err
		}
	}
	if cfg.Native.H3 == "" {
		return nil
	}
	return b.addH3()
}

func (b *listenerBuild) addTCP(l tcpListener) error {
	protocols := &http.Protocols{}
	protocols.SetHTTP1(l.alpn != "h2")
	protocols.SetHTTP2(l.alpn == "h2")
	var spa http.Handler
	if l.topo.spa {
		spa = b.spa
	}
	s := baseServer(b.authn.Enforce(newMux(b.ctx, b.e, l.topo, spa, b.authn), l.listener), protocols,
		b.e.controlTimeout, &b.peers)
	ln, err := b.sockets.listenTCP(l.addr)
	if err != nil {
		return err
	}
	b.opened = append(b.opened, ln)
	var served net.Listener = admittedListener{Listener: ln, admission: b.connections}
	if l.alpn != "" {
		served = tls.NewListener(served, b.cm.tlsConfig(l.alpn))
	}
	b.services = append(b.services, service{name: l.name, roles: l.roles, addr: l.addr, network: "tcp",
		run: func() error { return serve(served, s) }, stop: func(ctx context.Context) error {
			err := s.Shutdown(ctx)
			if err != nil {
				_ = s.Close()
			}
			return err
		}})
	return nil
}

func serveWebTransport(ctx context.Context, wt *webtransport.Server, ln *quic.Listener, peers *peerLog) error {
	for {
		conn, err := ln.Accept(ctx)
		if err != nil {
			return err
		}
		go func() {
			err := wt.ServeQUICConn(conn)
			var closed *quic.ApplicationError
			var idle *quic.IdleTimeoutError
			if err != nil && !errors.Is(err, http.ErrServerClosed) && !errors.As(err, &idle) &&
				!(errors.As(err, &closed) && closed.Remote) {
				peers.printf("webtransport connection: %q", err)
			}
		}()
	}
}

// peerLog holds connection failures any unauthenticated peer can cause to one line a minute.
type peerLog struct {
	mu         sync.Mutex
	next       time.Time
	suppressed int
}

func (p *peerLog) printf(format string, args ...any) {
	p.mu.Lock()
	defer p.mu.Unlock()
	now := time.Now()
	if now.Before(p.next) {
		p.suppressed++
		return
	}
	if p.suppressed > 0 {
		format += fmt.Sprintf(" (and %d more peer connection failures in the last minute)", p.suppressed)
	}
	p.next, p.suppressed = now.Add(time.Minute), 0
	logx.Infof("peer", format, args...)
}

// Write takes net/http's error log: panics and accept failures are the server's, the rest mostly a peer's doing.
func (p *peerLog) Write(b []byte) (int, error) {
	line := strings.TrimSuffix(string(b), "\n")
	if strings.Contains(line, "panic serving") || strings.HasPrefix(line, "http: Accept error") {
		logx.Errorf("http", "%s", line)
	} else {
		p.printf("%s", line)
	}
	return len(b), nil
}

// quicUse closes a handlerless connection: sessions-only at once, else after idle (a stalled stream stops H3's timer).
type quicUse struct {
	conn               *quic.Conn
	conns              *quicConns
	idle               time.Duration
	unused             *time.Timer
	mu                 sync.Mutex
	active             int
	tracked            bool
	sessions, requests bool
	linger             time.Duration // lets a server-ended session's close capsule reach its peer first
}

const wtCloseLinger = time.Second

type quicUseKey struct{}

func withQUICUse(idle time.Duration, conns *quicConns) func(context.Context, *quic.Conn) context.Context {
	return func(ctx context.Context, conn *quic.Conn) context.Context {
		u := &quicUse{conn: conn, conns: conns, idle: idle}
		u.unused = time.AfterFunc(idle, func() { u.closeIfIdle(true) })
		conns.add(conn)
		return context.WithValue(ctx, quicUseKey{}, u)
	}
}

func countQUICUse(next http.Handler) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		u, ok := r.Context().Value(quicUseKey{}).(*quicUse)
		if !ok {
			next.ServeHTTP(w, r)
			return
		}
		u.mu.Lock()
		u.active++
		u.unused.Stop()
		u.requests = u.requests || r.Method != http.MethodConnect
		u.mu.Unlock()
		defer u.leave()
		next.ServeHTTP(w, r)
	})
}

func (u *quicUse) leave() {
	u.mu.Lock()
	u.active--
	if u.active == 0 {
		u.unused.Reset(u.idle)
	}
	linger := u.linger
	u.mu.Unlock()
	if linger == 0 {
		u.closeIfIdle(false)
		return
	}
	time.AfterFunc(linger, func() { u.closeIfIdle(false) })
}

func (u *quicUse) closeIfIdle(unused bool) {
	u.mu.Lock()
	idle := u.active == 0 && (unused || u.sessions && !u.requests)
	u.mu.Unlock()
	if idle {
		_ = u.conn.CloseWithError(quic.ApplicationErrorCode(http3.ErrCodeNoError), "")
	}
}

// webTransportSession tracks r's connection before upgrade; ended records a successful session's close, cut closes it.
func webTransportSession(r *http.Request) (ended func(byPeer bool), cut func()) {
	u, ok := r.Context().Value(quicUseKey{}).(*quicUse)
	if !ok {
		return func(bool) {}, func() {}
	}
	u.mu.Lock()
	if !u.tracked {
		u.conns.trackSession(u.conn)
		u.tracked = true
	}
	u.mu.Unlock()
	return func(byPeer bool) {
		u.mu.Lock()
		u.sessions = true
		u.linger = 0
		if !byPeer {
			u.linger = wtCloseLinger
		}
		u.mu.Unlock()
	}, func() { _ = u.conn.CloseWithError(quic.ApplicationErrorCode(http3.ErrCodeNoError), "") }
}

// quicConns lets a shutdown end sessions with their cause, then close every connection with H3_NO_ERROR, not 0.
type quicConns struct {
	mu       sync.Mutex
	open     map[*quic.Conn]struct{}
	carrying int
	drained  chan struct{} // closed while carrying is zero
}

func newQUICConns() *quicConns {
	c := &quicConns{open: make(map[*quic.Conn]struct{}), drained: make(chan struct{})}
	close(c.drained)
	return c
}

func (c *quicConns) add(conn *quic.Conn) {
	c.mu.Lock()
	c.open[conn] = struct{}{}
	c.mu.Unlock()
	context.AfterFunc(conn.Context(), func() {
		c.mu.Lock()
		delete(c.open, conn)
		c.mu.Unlock()
	})
}

func (c *quicConns) trackSession(conn *quic.Conn) {
	c.mu.Lock()
	if c.carrying++; c.carrying == 1 {
		c.drained = make(chan struct{})
	}
	c.mu.Unlock()
	context.AfterFunc(conn.Context(), func() {
		c.mu.Lock()
		defer c.mu.Unlock()
		if c.carrying--; c.carrying == 0 {
			close(c.drained)
		}
	})
}

func (c *quicConns) close(ctx context.Context) {
	c.mu.Lock()
	drained := c.drained
	c.mu.Unlock()
	select {
	case <-drained:
	case <-ctx.Done():
	}
	c.mu.Lock()
	open := slices.Collect(maps.Keys(c.open))
	c.mu.Unlock()
	for _, conn := range open {
		_ = conn.CloseWithError(quic.ApplicationErrorCode(http3.ErrCodeNoError), "")
	}
}

func h3QUICConfig(cfg *config.Config) *quic.Config {
	q := transport.NewQUICConfig()
	q.HandshakeIdleTimeout = 5 * time.Second
	q.MaxIdleTimeout = wire.IdleBound
	// A request stream past the client's admission shares would pin its headers only to be refused.
	q.MaxIncomingStreams = int64(cfg.MaxActiveMeasurementsPerClient + cfg.MaxSessionsPerClient + h3ControlStreams)
	// Credit past the lane cap, so an excess lane is reset rather than parked (api/wire.md).
	q.MaxIncomingUniStreams = browserH3UniStreams + wire.WTMaxStreams + wtLaneCreditHeadroom
	return q
}

func (b *listenerBuild) addH3() error {
	quicConfig := h3QUICConfig(b.cfg)
	h3 := &http3.Server{Addr: b.cfg.Native.H3, TLSConfig: b.cm.tlsConfig(), QUICConfig: quicConfig}
	// Enforce has already bound a CONNECT's origin to its principal.
	wt := &webtransport.Server{H3: h3, CheckOrigin: func(*http.Request) bool { return true }}
	webtransport.ConfigureHTTP3Server(h3)
	h3.Handler = countQUICUse(boundedRequest(b.authn.Enforce(newMux(b.ctx, b.e, muxTopology{transfers: true, wt: wt},
		nil, b.authn), auth.Listener{WebTransport: true}), b.e.controlTimeout))
	conns := newQUICConns()
	h3.ConnContext = withQUICUse(b.e.controlTimeout, conns)
	h3.MaxHeaderBytes = h3MaxHeaderBytes
	pc, err := b.sockets.listenUDP(b.cfg.Native.H3)
	if err != nil {
		return err
	}
	b.opened = append(b.opened, pc)
	quicTransport := b.connections.quicTransport(pc)
	quicListener, err := quicTransport.Listen(http3.ConfigureTLSConfig(h3.TLSConfig), h3.QUICConfig)
	if err != nil {
		return err
	}
	b.services = append(b.services,
		service{name: "HTTP/3", roles: "probe, transfers, progress, WebTransport", addr: b.cfg.Native.H3,
			network: "udp",
			run: func() error {
				err := serveWebTransport(b.ctx, wt, quicListener, &b.peers)
				if errors.Is(err, http.ErrServerClosed) || errors.Is(err, net.ErrClosed) ||
					errors.Is(err, context.Canceled) {
					return nil
				}
				return err
			}, stop: func(ctx context.Context) error {
				conns.close(ctx)
				err := wt.Close()
				_ = quicListener.Close()
				_ = quicTransport.Close()
				return err
			}})
	return nil
}

func runServices(ctx context.Context, cfg *config.Config, services []service) error {
	errs := make(chan error, len(services))
	for _, svc := range services {
		// A table: where, which protocol, what for.
		logx.Infof("listen", "%-21s %-9s %s", svc.addr+"/"+svc.network, svc.name, svc.roles)
		go func() {
			err := svc.run()
			if err != nil {
				err = fmt.Errorf("%s on %s: %w", svc.name, svc.addr, err)
			}
			errs <- err
		}()
	}
	logx.Infof("server", "ready")
	defer func() {
		stopCtx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
		defer cancel()
		var wg sync.WaitGroup
		for _, svc := range services {
			wg.Go(func() { _ = svc.stop(stopCtx) })
		}
		wg.Wait()
		logx.Infof("server", "stopped")
	}()
	select {
	case <-ctx.Done():
		logx.Infof("server", "stop requested; closing listeners and draining connections")
		return nil
	case err := <-errs:
		return err
	}
}

func runAdmissionLog(ctx context.Context, requests *requestAdmission, connections *connectionAdmission) {
	ticker := time.Tick(30 * time.Second)
	for {
		select {
		case <-ctx.Done():
			return
		case <-ticker:
			r, s := requests.stats()
			c := connections.stats()
			// One line per limiter, so each reads at a glance and none wraps.
			logx.Infof("admission", "handlers %d active / %d peak, rejected %d pool + %d client",
				r.active, r.peak, r.rejectedGlobal, r.rejectedClient)
			logx.Infof("admission", "sessions %d active / %d max, %d per client, rejected %d budget + %d client",
				s.active, s.limit, s.clientLimit, s.rejectedGlobal, s.rejectedClient)
			logx.Infof("admission", "connections %d active / %d peak, rejected %d global + %d client",
				c.active, c.peak, c.rejectedGlobal, c.rejectedClient)
		}
	}
}

func serve(ln net.Listener, srv *http.Server) error {
	err := srv.Serve(ln)
	if errors.Is(err, http.ErrServerClosed) {
		return nil
	}
	return err
}
