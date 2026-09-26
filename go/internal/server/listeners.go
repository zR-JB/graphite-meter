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
	"slices"
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
	h2ReceiveWindowPerConnection     = 16 << 20
	h2ReceiveWindowPerStream         = 8 << 20
)

// endpoints is the one measurement core every listener mounts.
type endpoints struct {
	discovery             *endpoint.Discovery
	probe, bootstrapProbe *endpoint.Probe
	download              *endpoint.Download
	upload                *endpoint.Upload
	// WebTransport lanes run through these; tests wrap them.
	stream      endpoint.StreamFunc
	receive     endpoint.ReceiveFunc
	admission   *requestAdmission
	trusted     []netip.Prefix
	wtIdleBound time.Duration
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
		download:       download, stream: download.Stream,
		upload: upload, receive: upload.Receive,
		admission:   admission,
		trusted:     cfg.TrustedProxies,
		wtIdleBound: wire.WTIdleBound,
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

func baseServer(handler http.Handler, protocols *http.Protocols) *http.Server {
	return &http.Server{Handler: handler, ReadHeaderTimeout: 10 * time.Second, IdleTimeout: 60 * time.Second,
		MaxHeaderBytes: 32 << 10, Protocols: protocols, HTTP2: &http.HTTP2Config{
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
	if cfg.TLSEnabled() {
		if cm, err = newCertificateManager(cfg); err != nil {
			return nil, err
		}
		go cm.run(ctx)
	}
	e := buildEndpoints(ctx, cfg)
	connections := newConnectionAdmission(cfg.MaxConnections, cfg.MaxConnectionsPerClient, cfg.TrustedProxies)
	var spa http.Handler
	if authn.Enabled() {
		spa = static.AuthenticatedHandlerWithResultHistoryDefault(cfg.ResultHistoryDefault)
		authn.SetConnectOrigins(slices.Concat(e.discovery.ConnectOrigins(authn.PublicHostname()),
			cfg.ServerCatalog.ConnectSources()))
	} else {
		// Public pages use the same configured destination boundary as authenticated pages.
		page := static.HandlerWithResultHistoryDefault(cfg.ResultHistoryDefault)
		spa = http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
			w.Header().Set("Content-Security-Policy", e.discovery.PagePolicy(endpoint.RequestHost(r)))
			w.Header().Set("X-Frame-Options", "DENY")
			page.ServeHTTP(w, r)
		})
	}
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
	services    []service
	opened      []io.Closer
	sockets     listenerSockets
}

// tcpListener is one native TCP listener: its mux topology and, under TLS, its one ALPN protocol.
type tcpListener struct {
	name, addr, alpn string
	listener         auth.Listener
	topo             muxTopology
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
	h1Name := "HTTP/1.1 clear: UI, discovery, probe, transfers, WebSockets"
	if b.authn.Enabled() {
		h1Name = "HTTP/1.1 clear: trusted proxy upstream only; direct requests are refused, GET / redirects to HTTPS"
	}
	for _, l := range []tcpListener{
		{h1Name, cfg.Native.H1, "", auth.Listener{UI: true}, ui},
		{"HTTPS/WSS HTTP/1.1: UI, discovery, probe, transfers, WebSockets", cfg.Native.H1TLS, "http/1.1",
			auth.Listener{UI: true}, uiTLS},
		{"HTTPS HTTP/2: measurement probe, transfers, progress only", cfg.Native.H2, "h2",
			auth.Listener{}, muxTopology{transfers: true, requiredProto: 2}},
		{"HTTPS HTTP/1.1 companion: HTTP/3 bootstrap probe only", cfg.Native.H3, "http/1.1",
			auth.Listener{}, muxTopology{bootstrap: true}},
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
	s := baseServer(b.authn.Enforce(newMux(b.ctx, b.e, l.topo, spa, b.authn), l.listener), protocols)
	ln, err := b.sockets.listenTCP(l.addr)
	if err != nil {
		return err
	}
	b.opened = append(b.opened, ln)
	var served net.Listener = admittedListener{Listener: ln, admission: b.connections}
	if l.alpn != "" {
		served = tls.NewListener(served, b.cm.tlsConfig(l.alpn))
	}
	b.services = append(b.services, service{name: l.name, addr: l.addr, network: "tcp",
		run: func() error { return serve(served, s) }, stop: s.Shutdown})
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
	// Credit past the lane cap, so an excess lane is reset rather than parked (api/wire.md).
	cfg.MaxIncomingUniStreams = browserH3UniStreams + wire.WTMaxStreams + wtLaneCreditHeadroom
	return cfg
}

func (b *listenerBuild) addH3() error {
	quicConfig := h3QUICConfig()
	h3 := &http3.Server{Addr: b.cfg.Native.H3, TLSConfig: b.cm.tlsConfig(), QUICConfig: quicConfig}
	wt := &webtransport.Server{H3: h3, CheckOrigin: wtOriginCheck(b.authn)}
	webtransport.ConfigureHTTP3Server(h3)
	h3.Handler = b.authn.Enforce(newMux(b.ctx, b.e, muxTopology{transfers: true, wt: wt}, nil, b.authn),
		auth.Listener{WebTransport: true})
	h3.MaxHeaderBytes = 32 << 10
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
		service{name: "HTTP/3: probe, transfers, progress, WebTransport", addr: b.cfg.Native.H3, network: "udp",
			run: func() error {
				err := serveWebTransport(b.ctx, wt, quicListener)
				if errors.Is(err, http.ErrServerClosed) || errors.Is(err, net.ErrClosed) ||
					errors.Is(err, context.Canceled) {
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
			r, s := requests.stats()
			c := connections.stats()
			log.Printf("[gm:admission] handlers %d active / %d peak, rejected %d pool + %d client; "+
				"sessions %d active / %d max, %d per client, rejected %d budget + %d client; "+
				"connections %d active / %d peak, rejected %d global + %d client",
				r.active, r.peak, r.rejectedGlobal, r.rejectedClient,
				s.active, s.limit, s.clientLimit, s.rejectedGlobal, s.rejectedClient,
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
