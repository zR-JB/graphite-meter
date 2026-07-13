// Command graphite-meter is the measurement server: it serves the embedded
// Svelte client and the registered HTTP/WebSocket measurement endpoints over
// HTTP/1.1. See docs/ARCHITECTURE.md#roadmap for planned TLS/h2/h3 support.
package main

import (
	"context"
	"flag"
	"log"
	"os/signal"
	"syscall"

	"github.com/zR-JB/graphite-meter/go/internal/config"
	"github.com/zR-JB/graphite-meter/go/internal/server"
)

func main() {
	cfg, err := config.Load()
	if err != nil {
		log.Fatalf("configuration error: %v", err)
	}

	// Flags take final precedence over env/defaults.
	flag.StringVar(&cfg.H1Addr, "h1-addr", cfg.H1Addr, "HTTP/1.1 listen address")
	flag.StringVar(&cfg.H2Addr, "h2-addr", cfg.H2Addr, "HTTP/2 TLS listen address")
	flag.StringVar(&cfg.H3Addr, "h3-addr", cfg.H3Addr, "HTTP/3 UDP and bootstrap TCP listen address")
	flag.BoolVar(&cfg.EnableH2, "enable-h2", cfg.EnableH2, "enable native HTTP/2")
	flag.BoolVar(&cfg.EnableH3, "enable-h3", cfg.EnableH3, "enable native HTTP/3")
	flag.StringVar(&cfg.TLSCert, "tls-cert", cfg.TLSCert, "TLS certificate PEM path")
	flag.StringVar(&cfg.TLSKey, "tls-key", cfg.TLSKey, "TLS private key PEM path")
	flag.StringVar(&cfg.PublicH1Origin, "public-h1-origin", cfg.PublicH1Origin, "public HTTP/1.1 origin")
	flag.StringVar(&cfg.PublicH2Origin, "public-h2-origin", cfg.PublicH2Origin, "public HTTP/2 origin")
	flag.StringVar(&cfg.PublicH3Origin, "public-h3-origin", cfg.PublicH3Origin, "public HTTP/3 origin")
	flag.StringVar(&cfg.ServerName, "name", cfg.ServerName, "server name advertised in /preflight")
	flag.StringVar(&cfg.ServerLocation, "location", cfg.ServerLocation, "server location label")
	flag.BoolVar(&cfg.Verbose, "verbose", cfg.Verbose, "log per-second download/upload throughput")
	flag.Parse()
	if err := cfg.Validate(); err != nil {
		log.Fatalf("configuration error: %v", err)
	}

	ctx, stop := signal.NotifyContext(context.Background(), syscall.SIGINT, syscall.SIGTERM)
	defer stop()

	if err := server.Run(ctx, &cfg); err != nil {
		log.Fatalf("server error: %v", err)
	}
}
