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
	// webtransport "github.com/quic-go/webtransport-go" // Enable with the Stage 5 routes below.
	"github.com/zR-JB/graphite-meter/go/internal/config"
	"github.com/zR-JB/graphite-meter/go/internal/endpoint"
	"github.com/zR-JB/graphite-meter/go/internal/static"
	"github.com/zR-JB/graphite-meter/go/internal/transport"
)

const downloadBlockSize = 256 * 1024

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
	var dlMeter, ulMeter *endpoint.Meter
	if cfg.Verbose {
		dlMeter, ulMeter = endpoint.NewMeter("server:download"), endpoint.NewMeter("server:upload")
		go dlMeter.Run(ctx)
		go ulMeter.Run(ctx)
	}
	store := endpoint.NewUploadStore()
	go store.RunSweeper(ctx)
	h3Port := publicH3Port(cfg)
	return &endpoints{
		preflight: endpoint.NewPreflight(cfg), probe: endpoint.NewProbe(cfg, ""), bootstrapProbe: endpoint.NewProbe(cfg, h3Port),
		download: endpoint.NewDownload(block, dlMeter), uploadSession: endpoint.NewUploadSession(store), upload: endpoint.NewUpload(ulMeter, store, cfg.TrustedProxies),
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
	reg := endpoint.NewRegistry()
	if topology.discovery {
		reg.RegisterHTTP("/preflight", e.preflight)
	}
	probe := e.probe
	if topology.bootstrap {
		probe = e.bootstrapProbe
	}
	reg.RegisterHTTP("/probe", probe)
	if topology.transfers {
		transfer := func(h endpoint.Endpoint) endpoint.Endpoint {
			if topology.requiredProto == 0 {
				return h
			}
			return protocolEndpoint{Endpoint: h, major: topology.requiredProto}
		}
		reg.RegisterHTTP("/download", transfer(e.download))
		reg.RegisterHTTP("/upload/session", transfer(e.uploadSession))
		reg.RegisterHTTP("/upload", transfer(e.upload))
		reg.RegisterHTTP("/upload/progress", transfer(e.uploadProgress))
	}
	if topology.latency {
		reg.RegisterWS("/ws/ping", e.ping)
	}
	inner := http.NewServeMux()
	reg.Mount(ctx, inner)
	if topology.spa {
		inner.Handle("/", spa)
	}
	if e.admission == nil {
		return inner
	}
	m := http.NewServeMux()
	for _, path := range []string{"/download", "/upload", "/upload/progress", "/ws/ping"} {
		m.Handle(path, e.admission.wrap(inner, e.trustedProxies))
	}
	m.Handle("/", inner)
	return m
}

func baseServer(handler http.Handler, protocols *http.Protocols) *http.Server {
	return &http.Server{Handler: handler, ReadHeaderTimeout: 10 * time.Second, Protocols: protocols, ConnContext: func(ctx context.Context, c net.Conn) context.Context {
		if tc, ok := c.(*net.TCPConn); ok {
			_ = tc.SetNoDelay(true)
		}
		return ctx
	}}
}

// Run binds every configured listener only after certificate validation and
// shuts the logical server down as one unit.
func Run(ctx context.Context, cfg *config.Config) error {
	if err := cfg.Validate(); err != nil {
		return err
	}
	var cm *certificateManager
	var err error
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
	if cfg.Verbose {
		go runAdmissionLog(ctx, e.admission, connections)
	}
	h1p := &http.Protocols{}
	h1p.SetHTTP1(true)
	h1 := baseServer(listenerMux(ctx, e, muxTopology{spa: true, discovery: true, latency: true, transfers: true}), h1p)
	h1ln, err := net.Listen("tcp", cfg.Native.H1)
	if err != nil {
		return err
	}
	opened := []io.Closer{h1ln}
	closeOpened := func() {
		for _, c := range opened {
			_ = c.Close()
		}
	}
	services := []service{{
		name: "HTTP/1.1 clear: UI, discovery, probe, transfers, WebSockets", addr: cfg.Native.H1, network: "tcp",
		run: func() error { return serve(admittedListener{Listener: h1ln, admission: connections}, h1) }, stop: h1.Shutdown,
	}}

	if cfg.Native.H1TLS != "" {
		p := &http.Protocols{}
		p.SetHTTP1(true)
		s := baseServer(listenerMux(ctx, e, muxTopology{spa: true, discovery: true, latency: true, transfers: true, requiredProto: 1}), p)
		ln, err := net.Listen("tcp", cfg.Native.H1TLS)
		if err != nil {
			closeOpened()
			return err
		}
		opened = append(opened, ln)
		tlsLn := tls.NewListener(admittedListener{Listener: ln, admission: connections}, cm.tlsConfig("http/1.1"))
		services = append(services, service{
			name: "HTTPS/WSS HTTP/1.1: UI, discovery, probe, transfers, WebSockets",
			addr: cfg.Native.H1TLS, network: "tcp", run: func() error { return serve(tlsLn, s) }, stop: s.Shutdown,
		})
	}

	if cfg.Native.H2 != "" {
		p := &http.Protocols{}
		p.SetHTTP2(true)
		s := baseServer(listenerMux(ctx, e, muxTopology{transfers: true, requiredProto: 2}), p)
		ln, err := net.Listen("tcp", cfg.Native.H2)
		if err != nil {
			closeOpened()
			return err
		}
		opened = append(opened, ln)
		tlsLn := tls.NewListener(admittedListener{Listener: ln, admission: connections}, cm.tlsConfig("h2"))
		services = append(services, service{
			name: "HTTPS HTTP/2: measurement probe, transfers, progress only",
			addr: cfg.Native.H2, network: "tcp", run: func() error { return serve(tlsLn, s) }, stop: s.Shutdown,
		})
	}
	if cfg.Native.H3 != "" {
		p := &http.Protocols{}
		p.SetHTTP1(true)
		bootstrap := baseServer(listenerMux(ctx, e, muxTopology{bootstrap: true}), p)
		ln, err := net.Listen("tcp", cfg.Native.H3)
		if err != nil {
			closeOpened()
			return err
		}
		opened = append(opened, ln)
		tlsLn := tls.NewListener(admittedListener{Listener: ln, admission: connections}, cm.tlsConfig("http/1.1"))
		h3 := &http3.Server{Addr: cfg.Native.H3, TLSConfig: cm.tlsConfig(), QUICConfig: transport.NewQUICConfig(), Handler: listenerMux(ctx, e, muxTopology{transfers: true})}
		// webtransport.ConfigureHTTP3Server(h3) // Stage 5: enable with advertised WebTransport endpoints.
		pc, err := net.ListenPacket("udp", cfg.Native.H3)
		if err != nil {
			closeOpened()
			return err
		}
		opened = append(opened, pc)
		quicTransport := &quic.Transport{Conn: pc, ConnContext: connections.connContext}
		quicListener, err := quicTransport.Listen(http3.ConfigureTLSConfig(h3.TLSConfig), h3.QUICConfig)
		if err != nil {
			closeOpened()
			return err
		}
		services = append(services,
			service{
				name: "HTTPS HTTP/1.1 companion: HTTP/3 bootstrap probe only",
				addr: cfg.Native.H3, network: "tcp", run: func() error { return serve(tlsLn, bootstrap) }, stop: bootstrap.Shutdown,
			},
			service{name: "HTTP/3: probe, transfers, progress", addr: cfg.Native.H3, network: "udp", run: func() error {
				err := h3.ServeListener(quicListener)
				if errors.Is(err, http.ErrServerClosed) || errors.Is(err, net.ErrClosed) {
					return nil
				}
				return err
			}, stop: func(ctx context.Context) error {
				err := h3.Shutdown(ctx)
				_ = quicListener.Close()
				_ = quicTransport.Close()
				return err
			}})
	}

	runCtx, cancel := context.WithCancel(ctx)
	defer cancel()
	errs := make(chan error, len(services))
	for _, svc := range services {
		svc := svc
		log.Printf("graphite-meter %s listening on %s/%s (%s)", cfg.EngineVersion, svc.addr, svc.network, svc.name)
		go func() {
			if err := svc.run(); err != nil {
				errs <- fmt.Errorf("%s: %w", svc.name, err)
				cancel()
			} else {
				errs <- nil
			}
		}()
	}
	select {
	case <-runCtx.Done():
	case err := <-errs:
		if err != nil {
			cancel()
			shutdown(services)
			return err
		}
	}
	shutdown(services)
	return nil
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
