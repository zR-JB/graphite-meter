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
	"net/url"
	"time"

	"github.com/quic-go/quic-go"
	"github.com/quic-go/quic-go/http3"
	webtransport "github.com/quic-go/webtransport-go"
	"github.com/zR-JB/graphite-meter/go/internal/config"
	"github.com/zR-JB/graphite-meter/go/internal/endpoint"
	"github.com/zR-JB/graphite-meter/go/internal/static"
)

const downloadBlockSize = 256 * 1024

func BuildMux(ctx context.Context, reg *endpoint.Registry) *http.ServeMux {
	mux := http.NewServeMux()
	reg.Mount(ctx, mux)
	mux.Handle("/", static.Handler())
	return mux
}

type endpoints struct {
	preflight, probe, bootstrapProbe endpoint.Endpoint
	download, uploadSession, upload  endpoint.Endpoint
	ping, uploadProgress             endpoint.Endpoint
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
		download: endpoint.NewDownload(block, dlMeter), uploadSession: endpoint.NewUploadSession(store), upload: endpoint.NewUpload(ulMeter, store),
		ping: endpoint.NewPing(), uploadProgress: endpoint.NewUploadProgress(store),
	}, nil
}

func publicH3Port(cfg *config.Config) string {
	if cfg.PublicH3Origin != "" {
		u, err := url.Parse(cfg.PublicH3Origin)
		if err == nil {
			if port := u.Port(); port != "" {
				return port
			}
			return "443"
		}
	}
	_, port, _ := net.SplitHostPort(cfg.H3Addr)
	return port
}

type muxTopology struct {
	spa, discovery, latency bool
	requiredProto           int
}

func fullMux(ctx context.Context, e *endpoints, topology muxTopology) *http.ServeMux {
	m := http.NewServeMux()
	if topology.discovery {
		m.Handle("/preflight", endpoint.HTTPHandler(e.preflight))
	}
	m.Handle("/probe", endpoint.HTTPHandler(e.probe))
	transfer := func(h endpoint.Endpoint) http.Handler {
		base := endpoint.HTTPHandler(h)
		if topology.requiredProto == 0 {
			return base
		}
		return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
			if r.ProtoMajor != topology.requiredProto {
				http.NotFound(w, r)
				return
			}
			base.ServeHTTP(w, r)
		})
	}
	m.Handle("/download", transfer(e.download))
	m.Handle("/upload/session", transfer(e.uploadSession))
	m.Handle("/upload", transfer(e.upload))
	m.Handle("/upload/progress", transfer(e.uploadProgress))
	if topology.latency {
		m.Handle("/ws/ping", endpoint.WSHandler(ctx, e.ping))
	}
	if topology.spa {
		m.Handle("/", static.Handler())
	}
	return m
}

func bootstrapMux(e *endpoints) *http.ServeMux {
	m := http.NewServeMux()
	m.Handle("/probe", endpoint.HTTPHandler(e.bootstrapProbe))
	return m
}

func h3Mux(e *endpoints) *http.ServeMux {
	m := http.NewServeMux()
	m.Handle("/probe", endpoint.HTTPHandler(e.probe))
	m.Handle("/download", endpoint.HTTPHandler(e.download))
	m.Handle("/upload/session", endpoint.HTTPHandler(e.uploadSession))
	m.Handle("/upload", endpoint.HTTPHandler(e.upload))
	m.Handle("/upload/progress", endpoint.HTTPHandler(e.uploadProgress))
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
	if cfg.EnableH1TLS || cfg.EnableH2 || cfg.EnableH3 {
		if cm, err = newCertificateManager(cfg); err != nil {
			return err
		}
		go cm.run(ctx)
	}
	e, err := buildEndpoints(ctx, cfg)
	if err != nil {
		return err
	}
	h1p := &http.Protocols{}
	h1p.SetHTTP1(true)
	h1 := baseServer(fullMux(ctx, e, muxTopology{spa: true, discovery: true, latency: true}), h1p)
	h1ln, err := net.Listen("tcp", cfg.H1Addr)
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
		name: "HTTP/1.1 clear: UI, discovery, probe, transfers, WebSockets", addr: cfg.H1Addr, network: "tcp",
		run: func() error { return serve(h1ln, h1) }, stop: h1.Shutdown,
	}}

	if cfg.EnableH1TLS {
		p := &http.Protocols{}
		p.SetHTTP1(true)
		s := baseServer(fullMux(ctx, e, muxTopology{spa: true, discovery: true, latency: true, requiredProto: 1}), p)
		ln, err := net.Listen("tcp", cfg.H1TLSAddr)
		if err != nil {
			closeOpened()
			return err
		}
		opened = append(opened, ln)
		tlsLn := tls.NewListener(ln, cm.tlsConfig("http/1.1"))
		services = append(services, service{
			name: "HTTPS/WSS HTTP/1.1: UI, discovery, probe, transfers, WebSockets",
			addr: cfg.H1TLSAddr, network: "tcp", run: func() error { return serve(tlsLn, s) }, stop: s.Shutdown,
		})
	}

	if cfg.EnableH2 {
		p := &http.Protocols{}
		p.SetHTTP2(true)
		s := baseServer(fullMux(ctx, e, muxTopology{spa: true, discovery: true, requiredProto: 2}), p)
		ln, err := net.Listen("tcp", cfg.H2Addr)
		if err != nil {
			closeOpened()
			return err
		}
		opened = append(opened, ln)
		tlsLn := tls.NewListener(ln, cm.tlsConfig("h2"))
		services = append(services, service{
			name: "HTTPS HTTP/2: UI, discovery, probe, transfers, progress",
			addr: cfg.H2Addr, network: "tcp", run: func() error { return serve(tlsLn, s) }, stop: s.Shutdown,
		})
	}
	if cfg.EnableH3 {
		p := &http.Protocols{}
		p.SetHTTP1(true)
		bootstrap := baseServer(bootstrapMux(e), p)
		ln, err := net.Listen("tcp", cfg.H3Addr)
		if err != nil {
			closeOpened()
			return err
		}
		opened = append(opened, ln)
		tlsLn := tls.NewListener(ln, cm.tlsConfig("http/1.1"))
		h3 := &http3.Server{Addr: cfg.H3Addr, TLSConfig: cm.tlsConfig(), QUICConfig: &quic.Config{Allow0RTT: false}, Handler: h3Mux(e)}
		webtransport.ConfigureHTTP3Server(h3)
		pc, err := net.ListenPacket("udp", cfg.H3Addr)
		if err != nil {
			closeOpened()
			return err
		}
		opened = append(opened, pc)
		services = append(services,
			service{
				name: "HTTPS HTTP/1.1 companion: HTTP/3 bootstrap probe only",
				addr: cfg.H3Addr, network: "tcp", run: func() error { return serve(tlsLn, bootstrap) }, stop: bootstrap.Shutdown,
			},
			service{name: "HTTP/3: probe, transfers, progress", addr: cfg.H3Addr, network: "udp", run: func() error {
				err := h3.Serve(pc)
				if errors.Is(err, http.ErrServerClosed) || errors.Is(err, net.ErrClosed) {
					return nil
				}
				return err
			}, stop: func(ctx context.Context) error { err := h3.Shutdown(ctx); _ = pc.Close(); return err }})
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
