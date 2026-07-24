// Package server builds one measurement core and mounts it on protocol-specific listeners.
package server

import (
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
	"github.com/zR-JB/graphite-meter/go/internal/auth"
	"github.com/zR-JB/graphite-meter/go/internal/config"
	"github.com/zR-JB/graphite-meter/go/internal/endpoint"
	"github.com/zR-JB/graphite-meter/go/internal/static"
	"github.com/zR-JB/graphite-meter/go/internal/transport"
)

const downloadBlockSize = 256 * 1024

// Measurement route paths, the mounting half of the cross-language pin.
// /preflight advertises origins only, so every language keeps its own table.
// The other two are client/src/lib/runner/real/backendPure.ts (ROUTES) and
// wire.Default{Throughput,Latency}Routes. api/routes.txt pins all three.
const (
	routeProbe          = "/probe"
	routeDownload       = "/download"
	routeUpload         = "/upload"
	routeUploadSession  = "/upload/session"
	routeUploadProgress = "/upload/progress"
	routePing           = "/ws/ping"
)

type endpoints struct {
	preflight, probe, bootstrapProbe endpoint.Endpoint
	download, uploadSession, upload  endpoint.Endpoint
	ping, uploadProgress             endpoint.Endpoint
	admission                        *requestAdmission
	trustedProxies                   []netip.Prefix
}

type service struct {
	name, addr, network string
	run                 func() error
	stop                func(context.Context) error
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
	return &endpoints{
		preflight: endpoint.NewPreflight(cfg), probe: endpoint.NewProbe(cfg, ""), bootstrapProbe: endpoint.NewProbe(cfg, h3Port),
		download: endpoint.NewDownload(block, downloadMeter), uploadSession: endpoint.NewUploadSession(store), upload: endpoint.NewUpload(uploadMeter, store, cfg.TrustedProxies),
		ping: endpoint.NewPing(), uploadProgress: endpoint.NewUploadProgress(store, cfg.TrustedProxies),
		admission:      newRequestAdmission(cfg.MaxActiveMeasurements, cfg.MaxActiveMeasurementsPerClient, cfg.MaxOperationDuration),
		trustedProxies: cfg.TrustedProxies,
	}, nil
}

func publicH3Port(cfg *config.Config) string {
	if cfg.NativePublic.H3 != "" {
		u, err := url.Parse(cfg.NativePublic.H3)
		if err == nil {
			if port := u.Port(); port != "" {
				return port
			}
			return "443"
		}
	}
	// Without a public origin the advertised port is the one H3 binds.
	_, port, _ := net.SplitHostPort(cfg.Native.H3)
	return port
}

type muxTopology struct {
	spa, discovery, latency, transfers, bootstrap bool
	requiredProto                                 int
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

func listenerMux(ctx context.Context, e *endpoints, topology muxTopology) *http.ServeMux {
	return listenerMuxWithSPA(ctx, e, topology, static.Handler())
}

func listenerMuxWithSPA(ctx context.Context, e *endpoints, topology muxTopology, spa http.Handler) *http.ServeMux {
	return listenerMuxConfigured(ctx, e, topology, spa, nil)
}

func listenerMuxConfigured(ctx context.Context, e *endpoints, topology muxTopology, spa http.Handler, authn *auth.Service) *http.ServeMux {
	reg := endpoint.NewRegistry()
	if topology.discovery {
		reg.RegisterHTTP("/preflight", e.preflight)
	}
	probe := e.probe
	if topology.bootstrap {
		probe = e.bootstrapProbe
	}
	reg.RegisterHTTP(routeProbe, probe)
	if topology.transfers {
		transfer := func(h endpoint.Endpoint) endpoint.Endpoint {
			if topology.requiredProto == 0 {
				return h
			}
			return protocolEndpoint{Endpoint: h, major: topology.requiredProto}
		}
		reg.RegisterHTTP(routeDownload, transfer(e.download))
		reg.RegisterHTTP(routeUploadSession, transfer(e.uploadSession))
		reg.RegisterHTTP(routeUpload, transfer(e.upload))
		reg.RegisterHTTP(routeUploadProgress, transfer(e.uploadProgress))
	}
	if topology.latency {
		reg.RegisterWS(routePing, e.ping)
	}
	inner := http.NewServeMux()
	if authn != nil && authn.Enabled() {
		reg.MountWithOrigin(ctx, inner, authn.PublicOrigin())
	} else {
		reg.Mount(ctx, inner)
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
	m := http.NewServeMux()
	for _, path := range []string{routeDownload, routeUpload, routeUploadProgress, routePing} {
		m.Handle(path, e.admission.wrap(inner, e.trustedProxies, publicOrigin))
	}
	m.Handle("/", inner)
	return m
}

// disableNagle stops Nagle batching a small latency probe into the following
// write and inflating every sample. A connection that refuses it still measures.
func disableNagle(c net.Conn) {
	if tc, ok := c.(*net.TCPConn); ok {
		_ = tc.SetNoDelay(true)
	}
}

func baseServer(handler http.Handler, protocols *http.Protocols) *http.Server {
	return &http.Server{Handler: handler, ReadHeaderTimeout: 10 * time.Second, Protocols: protocols, ConnContext: func(ctx context.Context, c net.Conn) context.Context {
		disableNagle(c)
		return ctx
	}}
}

func hardenAuthenticatedServer(s *http.Server, enabled bool) {
	if enabled {
		s.IdleTimeout = 60 * time.Second
		s.MaxHeaderBytes = 32 << 10
	}
}

// pinConnectOrigins restricts the authenticated CSP's connect-src to the
// measurement origins /preflight advertises for the canonical UI host, the only
// host auth accepts. It derives them from the targets the client is handed, so
// it cannot omit one.
func pinConnectOrigins(cfg *config.Config, authn *auth.Service) {
	u, err := url.Parse(cfg.Auth.PublicURL)
	if err != nil {
		return
	}
	authn.SetConnectOrigins(endpoint.NewPreflight(cfg).ConnectOrigins(u.Hostname()))
}

// Run validates the config and certificate, binds every configured listener,
// and shuts the logical server down as one unit.
func Run(ctx context.Context, cfg *config.Config) error {
	if err := cfg.Validate(); err != nil {
		return err
	}
	authn, err := auth.New(ctx, cfg.Auth, cfg.TrustedProxies, cfg.Verbose)
	if err != nil {
		return err
	}
	var cm *certificateManager
	if cfg.Native.H1TLS != "" || cfg.Native.H2 != "" || cfg.Native.H3 != "" {
		if cm, err = newCertificateManager(cfg); err != nil {
			return err
		}
		go cm.run(ctx)
	}
	e, err := buildEndpoints(ctx, cfg)
	if err != nil {
		return err
	}
	connections := newConnectionAdmission(cfg.MaxConnections, cfg.MaxConnectionsPerClient, cfg.TrustedProxies)
	spa := static.Handler()
	if authn.Enabled() {
		spa = static.AuthenticatedHandler()
		pinConnectOrigins(cfg, authn)
	}
	if cfg.Verbose {
		go runAdmissionLog(ctx, e.admission, connections)
	}
	b := &listenerBuild{ctx: ctx, cfg: cfg, e: e, authn: authn, cm: cm, connections: connections}
	if err := b.assemble(spa); err != nil {
		return err
	}
	return runServices(ctx, cfg, b.services)
}

// listenerBuild accumulates the listeners Run assembles: the shared
// dependencies, the services to start, and the sockets to close if a later
// listener fails to bind.
type listenerBuild struct {
	ctx         context.Context
	cfg         *config.Config
	e           *endpoints
	authn       *auth.Service
	cm          *certificateManager
	connections *connectionAdmission
	services    []service
	opened      []io.Closer
}

// closeOpened releases every socket bound so far, so a failed bind does not
// leave the earlier listeners holding their ports.
func (b *listenerBuild) closeOpened() {
	for _, c := range b.opened {
		// Nothing to recover from: the process is already unwinding a bind failure.
		_ = c.Close()
	}
}

// tcpTLS binds a TLS-over-TCP listener and appends it as a service. The HTTPS
// H1, H2, and H3-bootstrap listeners share it.
func (b *listenerBuild) tcpTLS(name, addr string, proto *http.Protocols, l auth.Listener, topo muxTopology, handler http.Handler, alpn string) error {
	mux := listenerMuxConfigured(b.ctx, b.e, topo, handler, b.authn)
	s := baseServer(b.authn.Enforce(mux, l), proto)
	hardenAuthenticatedServer(s, b.authn.Enabled())
	ln, err := net.Listen("tcp", addr)
	if err != nil {
		b.closeOpened()
		return err
	}
	b.opened = append(b.opened, ln)
	tlsLn := tls.NewListener(admittedListener{Listener: ln, admission: b.connections}, b.cm.tlsConfig(alpn))
	b.services = append(b.services, service{
		name: name, addr: addr, network: "tcp",
		run: func() error { return serve(tlsLn, s) }, stop: s.Shutdown,
	})
	return nil
}

func h1Protocols() *http.Protocols {
	p := &http.Protocols{}
	p.SetHTTP1(true)
	return p
}

// assemble builds every configured listener into b.services.
func (b *listenerBuild) assemble(spa http.Handler) error {
	// Clear HTTP/1.1 is the one listener bound unconditionally.
	h1 := baseServer(b.authn.Enforce(listenerMuxConfigured(b.ctx, b.e, muxTopology{spa: true, discovery: true, latency: true, transfers: true}, spa, b.authn), auth.Listener{UI: true}), h1Protocols())
	hardenAuthenticatedServer(h1, b.authn.Enabled())
	h1ln, err := net.Listen("tcp", b.cfg.Native.H1)
	if err != nil {
		return err
	}
	b.opened = append(b.opened, h1ln)
	h1Name := "HTTP/1.1 clear: UI, discovery, probe, transfers, WebSockets"
	if b.authn.Enabled() {
		h1Name = "HTTP/1.1 clear: trusted proxy upstream only; direct requests are refused, GET / redirects to HTTPS"
	}
	b.services = append(b.services, service{
		name: h1Name, addr: b.cfg.Native.H1, network: "tcp",
		run: func() error { return serve(admittedListener{Listener: h1ln, admission: b.connections}, h1) }, stop: h1.Shutdown,
	})

	if b.cfg.Native.H1TLS != "" {
		if err := b.tcpTLS("HTTPS/WSS HTTP/1.1: UI, discovery, probe, transfers, WebSockets", b.cfg.Native.H1TLS, h1Protocols(), auth.Listener{UI: true}, muxTopology{spa: true, discovery: true, latency: true, transfers: true, requiredProto: 1}, spa, "http/1.1"); err != nil {
			return err
		}
	}
	if b.cfg.Native.H2 != "" {
		p := &http.Protocols{}
		p.SetHTTP2(true)
		if err := b.tcpTLS("HTTPS HTTP/2: measurement probe, transfers, progress only", b.cfg.Native.H2, p, auth.Listener{}, muxTopology{transfers: true, requiredProto: 2}, static.Handler(), "h2"); err != nil {
			return err
		}
	}
	if b.cfg.Native.H3 != "" {
		return b.assembleH3()
	}
	return nil
}

// assembleH3 binds the HTTP/3 UDP listener plus its TCP Alt-Svc bootstrap
// companion.
func (b *listenerBuild) assembleH3() error {
	if err := b.tcpTLS("HTTPS HTTP/1.1 companion: HTTP/3 bootstrap probe only", b.cfg.Native.H3, h1Protocols(), auth.Listener{}, muxTopology{bootstrap: true}, static.Handler(), "http/1.1"); err != nil {
		return err
	}
	h3mux := listenerMuxConfigured(b.ctx, b.e, muxTopology{transfers: true}, static.Handler(), b.authn)
	quicConfig := transport.NewQUICConfig()
	if b.authn.Enabled() {
		quicConfig.HandshakeIdleTimeout = 10 * time.Second
		quicConfig.MaxIdleTimeout = 60 * time.Second
		quicConfig.MaxIncomingStreams = 256
		quicConfig.MaxIncomingUniStreams = 32
	}
	h3 := &http3.Server{Addr: b.cfg.Native.H3, TLSConfig: b.cm.tlsConfig(), QUICConfig: quicConfig, Handler: b.authn.Enforce(h3mux, auth.Listener{})}
	if b.authn.Enabled() {
		h3.MaxHeaderBytes = 32 << 10
	}
	pc, err := net.ListenPacket("udp", b.cfg.Native.H3)
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
	b.services = append(b.services, service{name: "HTTP/3: probe, transfers, progress", addr: b.cfg.Native.H3, network: "udp",
		run: func() error {
			err := h3.ServeListener(quicListener)
			if errors.Is(err, http.ErrServerClosed) || errors.Is(err, net.ErrClosed) {
				return nil
			}
			return err
		}, stop: func(ctx context.Context) error {
			err := h3.Shutdown(ctx)
			// The listener and transport close unconditionally to free the UDP
			// socket. The graceful shutdown result is the one worth reporting.
			_ = quicListener.Close()
			_ = quicTransport.Close()
			return err
		}})
	return nil
}

// runServices starts every listener, then blocks until the context ends or a
// listener fails, shutting the rest down on the way out.
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
	defer shutdown(services)
	select {
	case <-ctx.Done():
		return nil
	case err := <-errs:
		return err
	}
}

func runAdmissionLog(ctx context.Context, requests *requestAdmission, connections *connectionAdmission) {
	ticker := time.NewTicker(30 * time.Second)
	defer ticker.Stop()
	for {
		select {
		case <-ctx.Done():
			return
		case <-ticker.C:
			r, c := requests.stats(), connections.stats()
			log.Printf("[gm:admission] handlers %d active / %d peak, rejected %d global + %d client; connections %d active / %d peak, rejected %d global + %d client",
				r.active, r.peak, r.rejectedGlobal, r.rejectedClient,
				c.active, c.peak, c.rejectedGlobal, c.rejectedClient)
		}
	}
}

// shutdown gives every listener the same bounded budget to drain. Failures are
// discarded: the caller is already returning, and one listener refusing to stop
// must not keep the others running.
func shutdown(services []service) {
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	for _, s := range services {
		_ = s.stop(ctx)
	}
}

func serve(ln net.Listener, srv *http.Server) error {
	if err := srv.Serve(ln); err != nil && !errors.Is(err, http.ErrServerClosed) {
		return err
	}
	return nil
}
