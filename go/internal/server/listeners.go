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
	"net"
	"net/http"
	"net/netip"
	"net/url"
	"time"

	"github.com/quic-go/quic-go"
	"github.com/quic-go/quic-go/http3"
	"github.com/quic-go/webtransport-go"
	"github.com/zR-JB/graphite-meter/go/internal/auth"
	"github.com/zR-JB/graphite-meter/go/internal/config"
	"github.com/zR-JB/graphite-meter/go/internal/endpoint"
	"github.com/zR-JB/graphite-meter/go/internal/static"
	"github.com/zR-JB/graphite-meter/go/internal/transport"
	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

const (
	downloadBlockSize                = 256 * 1024
	h3MaxTransferStreamsPerDirection = 128
	h3UploadProgressStreams          = 1
	h3MaxIncomingStreams             = 2*h3MaxTransferStreamsPerDirection + h3UploadProgressStreams
	browserH3UniStreams              = 3
	wtLaneCreditHeadroom             = 4
)

const (
	routeProbe          = "/probe"
	routeDownload       = "/download"
	routeUpload         = "/upload"
	routeUploadSession  = "/upload/session"
	routeUploadProgress = "/upload/progress"
	routeWTSession      = "/wt/session"
	routePing           = "/ws/ping"
	routeWTDownload     = "/wt/download"
	routeWTUpload       = "/wt/upload"
	routeWTPing         = "/wt/ping"
)

type endpoints struct {
	preflight, probe, bootstrapProbe endpoint.Endpoint
	download, uploadSession, upload  endpoint.Endpoint
	ping                             endpoint.Endpoint
	uploadProgress                   *endpoint.UploadProgress
	admission                        *requestAdmission
	trustedProxies                   []netip.Prefix
	wtIdleBound                      time.Duration
}

type service struct {
	name, addr, network string
	run                 func() error
	stop                func(context.Context) error
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

func buildEndpoints(ctx context.Context, cfg *config.Config) (*endpoints, error) {
	block := make([]byte, downloadBlockSize)
	if _, err := rand.Read(block); err != nil {
		return nil, err
	}
	var downloadMeter, uploadMeter *endpoint.Meter
	if cfg.Verbose {
		downloadMeter, uploadMeter = endpoint.NewMeter("server:download"), endpoint.NewMeter("server:upload")
		go downloadMeter.Run(ctx)
		go uploadMeter.Run(ctx)
	}
	store := endpoint.NewUploadStore()
	go store.RunSweeper(ctx)
	h3Port := publicH3Port(cfg)
	admission := newRequestAdmission(cfg.MaxActiveMeasurements, cfg.MaxActiveMeasurementsPerClient, cfg.MaxActiveSessions, cfg.MaxSessionsPerClient, cfg.MaxOperationDuration, cfg.MaxSessionDuration)
	return &endpoints{
		preflight: endpoint.NewPreflight(cfg), probe: endpoint.NewProbe(cfg, "", admission.load), bootstrapProbe: endpoint.NewProbe(cfg, h3Port, admission.load),
		download: endpoint.NewDownload(block, downloadMeter), uploadSession: endpoint.NewUploadSession(store), upload: endpoint.NewUpload(uploadMeter, store, cfg.TrustedProxies),
		ping: endpoint.NewPing(), uploadProgress: endpoint.NewUploadProgress(store, cfg.TrustedProxies),
		admission:      admission,
		trustedProxies: cfg.TrustedProxies,
		wtIdleBound:    wire.WTIdleBound,
	}, nil
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

type muxTopology struct {
	spa, discovery, latency, transfers, bootstrap bool
	requiredProto                                 int
	wt                                            *webtransport.Server
}

type protocolEndpoint struct {
	endpoint.Endpoint
	major int
}

func (e protocolEndpoint) Handle(s transport.Session) error {
	w, r, ok := s.HTTP()
	if !ok {
		return transport.ErrUnsupported
	}
	if r.ProtoMajor != e.major {
		http.NotFound(w, r)
		return nil
	}
	return e.Endpoint.Handle(s)
}

func buildRegistry(e *endpoints, topology muxTopology, authn *auth.Service) *endpoint.Registry {
	reg := endpoint.NewRegistry()
	if topology.discovery {
		reg.RegisterHTTP("/preflight", e.preflight)
	}
	if topology.bootstrap {
		reg.RegisterHTTP(routeProbe, e.bootstrapProbe)
	} else {
		reg.RegisterHTTP(routeProbe, e.probe)
	}
	if topology.transfers {
		register := func(path string, h endpoint.Endpoint) {
			if topology.requiredProto != 0 {
				h = protocolEndpoint{Endpoint: h, major: topology.requiredProto}
			}
			reg.RegisterHTTP(path, h)
		}
		register(routeDownload, e.download)
		register(routeUploadSession, e.uploadSession)
		var minter endpoint.WTTokenMinter
		if authn != nil && authn.Enabled() {
			minter = authn.MintWebTransportSessionToken
		}
		register(routeWTSession, endpoint.NewWTSession(minter))
		register(routeUpload, e.upload)
		register(routeUploadProgress, e.uploadProgress)
	}
	if topology.latency {
		reg.RegisterWS(routePing, e.ping)
	}
	if topology.wt != nil {
		reg.RegisterWT(routeWTDownload, endpoint.NewWTDownload(e.download, e.wtIdleBound))
		reg.RegisterWT(routeWTUpload, endpoint.NewWTUpload(e.upload, e.uploadProgress, e.trustedProxies, e.wtIdleBound))
		reg.RegisterWT(routeWTPing, endpoint.NewWTPing(e.ping, e.wtIdleBound))
	}
	return reg
}

func listenerMuxConfigured(ctx context.Context, e *endpoints, topology muxTopology, spa http.Handler, authn *auth.Service) *http.ServeMux {
	reg := buildRegistry(e, topology, authn)
	inner := http.NewServeMux()
	if authn != nil && authn.Enabled() {
		reg.MountWithOrigin(ctx, inner, authn.PublicOrigin())
	} else {
		reg.Mount(ctx, inner)
	}
	if topology.wt != nil {
		reg.MountWebTransport(ctx, inner, topology.wt)
	}
	if topology.spa && authn != nil {
		authn.Mount(inner)
	}
	if topology.spa {
		inner.Handle("/", spa)
	}
	if e.admission == nil {
		return inner
	}
	var publicOrigin string
	if authn != nil {
		publicOrigin = authn.PublicOrigin()
	}
	var wrapped []string
	if topology.transfers {
		wrapped = append(wrapped, routeDownload, routeUpload, routeUploadProgress)
	}
	if topology.latency {
		wrapped = append(wrapped, routePing)
	}
	if topology.wt != nil {
		wrapped = append(wrapped, routeWTDownload, routeWTUpload, routeWTPing)
	}
	m := http.NewServeMux()
	for _, path := range wrapped {
		m.Handle(path, e.admission.wrap(inner, e.trustedProxies, publicOrigin))
	}
	m.Handle("/", inner)
	return m
}

func wtOriginCheck(authn *auth.Service) func(*http.Request) bool {
	enabled := authn != nil && authn.Enabled()
	pinned := ""
	if enabled {
		pinned = authn.PublicOrigin()
	}
	return func(r *http.Request) bool {
		return !enabled || r.Header.Get("Origin") == "" || r.Header.Get("Origin") == pinned
	}
}

func baseServer(handler http.Handler, protocols *http.Protocols) *http.Server {
	return &http.Server{Handler: handler, ReadHeaderTimeout: 10 * time.Second, IdleTimeout: 60 * time.Second, MaxHeaderBytes: 32 << 10, Protocols: protocols, ConnContext: func(ctx context.Context, c net.Conn) context.Context {
		if tc, ok := c.(*net.TCPConn); ok {
			_ = tc.SetNoDelay(true)
		}
		return ctx
	}}
}

// Run validates the config and certificate, binds every configured listener.
func Run(ctx context.Context, cfg *config.Config) error {
	return runWithSockets(ctx, cfg, systemListenerSockets{})
}

func runWithSockets(ctx context.Context, cfg *config.Config, sockets listenerSockets) error {
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
	if cfg.Native.H1TLS != "" || cfg.Native.H2 != "" || cfg.Native.H3 != "" {
		if cm, err = newCertificateManager(cfg); err != nil {
			return nil, err
		}
		go cm.run(ctx)
	}
	e, err := buildEndpoints(ctx, cfg)
	if err != nil {
		return nil, err
	}
	connections := newConnectionAdmission(cfg.MaxConnections, cfg.MaxConnectionsPerClient, cfg.TrustedProxies)
	var spa http.Handler
	if authn.Enabled() {
		spa = static.AuthenticatedHandlerWithResultHistoryDefault(cfg.ResultHistoryDefault)
		if u, err := url.Parse(cfg.Auth.PublicURL); err == nil {
			authn.SetConnectOrigins(endpoint.NewPreflight(cfg).ConnectOrigins(u.Hostname()))
		}
	} else {
		spa = static.HandlerWithResultHistoryDefault(cfg.ResultHistoryDefault)
	}
	if cfg.Verbose {
		go runAdmissionLog(ctx, e.admission, connections)
	}
	return &listenerBuild{ctx: ctx, cfg: cfg, e: e, authn: authn, cm: cm, connections: connections, spa: spa, sockets: sockets}, nil
}

type listenerBuild struct {
	ctx         context.Context
	cfg         *config.Config
	e           *endpoints
	authn       *auth.Service
	cm          *certificateManager
	connections *connectionAdmission
	spa         http.Handler
	services    []service
	opened      []io.Closer
	sockets     listenerSockets
}

func (b *listenerBuild) closeOpened() {
	for _, c := range b.opened {
		_ = c.Close()
	}
}

func (b *listenerBuild) addTCP(name, addr string, proto *http.Protocols, l auth.Listener, topo muxTopology, handler http.Handler, alpn string) error {
	mux := listenerMuxConfigured(b.ctx, b.e, topo, handler, b.authn)
	s := baseServer(b.authn.Enforce(mux, l), proto)
	ln, err := b.sockets.listenTCP(addr)
	if err != nil {
		b.closeOpened()
		return err
	}
	b.opened = append(b.opened, ln)
	var served net.Listener = admittedListener{Listener: ln, admission: b.connections}
	if alpn != "" {
		served = tls.NewListener(served, b.cm.tlsConfig(alpn))
	}
	b.services = append(b.services, service{
		name: name, addr: addr, network: "tcp",
		run: func() error { return serve(served, s) }, stop: s.Shutdown,
	})
	return nil
}

func h1Protocols() *http.Protocols {
	p := &http.Protocols{}
	p.SetHTTP1(true)
	return p
}

func (b *listenerBuild) assemble() error {
	spa := b.spa
	h1Name := "HTTP/1.1 clear: UI, discovery, probe, transfers, WebSockets"
	if b.authn.Enabled() {
		h1Name = "HTTP/1.1 clear: trusted proxy upstream only; direct requests are refused, GET / redirects to HTTPS"
	}
	if err := b.addTCP(h1Name, b.cfg.Native.H1, h1Protocols(), auth.Listener{UI: true}, muxTopology{spa: true, discovery: true, latency: true, transfers: true}, spa, ""); err != nil {
		return err
	}

	if b.cfg.Native.H1TLS != "" {
		if err := b.addTCP("HTTPS/WSS HTTP/1.1: UI, discovery, probe, transfers, WebSockets", b.cfg.Native.H1TLS, h1Protocols(), auth.Listener{UI: true}, muxTopology{spa: true, discovery: true, latency: true, transfers: true, requiredProto: 1}, spa, "http/1.1"); err != nil {
			return err
		}
	}
	if b.cfg.Native.H2 != "" {
		p := &http.Protocols{}
		p.SetHTTP2(true)
		if err := b.addTCP("HTTPS HTTP/2: measurement probe, transfers, progress only", b.cfg.Native.H2, p, auth.Listener{}, muxTopology{transfers: true, requiredProto: 2}, static.Handler(), "h2"); err != nil {
			return err
		}
	}
	if b.cfg.Native.H3 != "" {
		return b.assembleH3()
	}
	return nil
}

func serveWebTransport(ctx context.Context, wt *webtransport.Server, ln *quic.Listener) error {
	for {
		conn, err := ln.Accept(ctx)
		if err != nil {
			return err
		}
		go func() {
			if err := wt.ServeQUICConn(conn); err != nil && !errors.Is(err, http.ErrServerClosed) {
				log.Printf("[gm:h3] webtransport connection: %v", err)
			}
		}()
	}
}

func h3QUICConfig() *quic.Config {
	cfg := transport.NewQUICConfig()
	cfg.HandshakeIdleTimeout = 5 * time.Second
	cfg.MaxIdleTimeout = 30 * time.Second
	cfg.MaxIncomingStreams = h3MaxIncomingStreams
	cfg.MaxIncomingUniStreams = browserH3UniStreams + wire.WTMaxStreams + wtLaneCreditHeadroom
	return cfg
}

func (b *listenerBuild) assembleH3() error {
	if err := b.addTCP("HTTPS HTTP/1.1 companion: HTTP/3 bootstrap probe only", b.cfg.Native.H3, h1Protocols(), auth.Listener{}, muxTopology{bootstrap: true}, static.Handler(), "http/1.1"); err != nil {
		return err
	}
	quicConfig := h3QUICConfig()
	h3 := &http3.Server{Addr: b.cfg.Native.H3, TLSConfig: b.cm.tlsConfig(), QUICConfig: quicConfig}
	wt := &webtransport.Server{H3: h3, CheckOrigin: wtOriginCheck(b.authn)}
	webtransport.ConfigureHTTP3Server(h3)
	h3.Handler = b.authn.Enforce(listenerMuxConfigured(b.ctx, b.e, muxTopology{transfers: true, wt: wt}, static.Handler(), b.authn), auth.Listener{WebTransport: true})
	h3.MaxHeaderBytes = 32 << 10
	pc, err := b.sockets.listenUDP(b.cfg.Native.H3)
	if err != nil {
		b.closeOpened()
		return err
	}
	b.opened = append(b.opened, pc)
	quicTransport := &quic.Transport{Conn: pc, ConnContext: b.connections.connContext}
	quicListener, err := quicTransport.Listen(http3.ConfigureTLSConfig(h3.TLSConfig), h3.QUICConfig)
	if err != nil {
		b.closeOpened()
		return err
	}
	b.services = append(b.services, service{name: "HTTP/3: probe, transfers, progress, WebTransport", addr: b.cfg.Native.H3, network: "udp",
		run: func() error {
			err := serveWebTransport(b.ctx, wt, quicListener)
			if errors.Is(err, http.ErrServerClosed) || errors.Is(err, net.ErrClosed) || errors.Is(err, context.Canceled) {
				return nil
			}
			return err
		}, stop: func(context.Context) error {
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
		log.Printf("graphite-meter %s listening on %s/%s (%s)", cfg.EngineVersion, svc.addr, svc.network, svc.name)
		go func() {
			err := svc.run()
			if err != nil {
				err = fmt.Errorf("%s: %w", svc.name, err)
			}
			errs <- err
		}()
	}
	defer func() {
		stopCtx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
		defer cancel()
		for _, svc := range services {
			_ = svc.stop(stopCtx)
		}
	}()
	select {
	case <-ctx.Done():
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
			log.Print(admissionLogLine(requests.stats(), connections.stats()))
		}
	}
}

func admissionLogLine(r requestAdmissionStats, c admissionStats) string {
	return fmt.Sprintf("[gm:admission] handlers %d active / %d peak, rejected %d pool + %d client; sessions %d active / %d max, %d per client, rejected %d budget + %d client; connections %d active / %d peak, rejected %d global + %d client",
		r.active, r.peak, r.rejectedGlobal, r.rejectedClient,
		r.activeSessions, r.sessionMax, r.sessionClientMax, r.rejectedSessionBudget, r.rejectedSessionClient,
		c.active, c.peak, c.rejectedGlobal, c.rejectedClient)
}

func serve(ln net.Listener, srv *http.Server) error {
	err := srv.Serve(ln)
	if errors.Is(err, http.ErrServerClosed) {
		return nil
	}
	return err
}
