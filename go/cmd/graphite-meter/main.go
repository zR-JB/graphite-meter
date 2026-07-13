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

	registerFlags(flag.CommandLine, &cfg)
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

func registerFlags(fs *flag.FlagSet, cfg *config.Config) {
	// Both names bind the same value; the last occurrence wins when both are used.
	fs.StringVar(&cfg.H1Addr, "addr", cfg.H1Addr, "HTTP/1.1 listen address (legacy alias)")
	fs.StringVar(&cfg.H1Addr, "h1-addr", cfg.H1Addr, "HTTP/1.1 listen address")
	fs.StringVar(&cfg.H2Addr, "h2-addr", cfg.H2Addr, "HTTP/2 TLS listen address")
	fs.StringVar(&cfg.H3Addr, "h3-addr", cfg.H3Addr, "HTTP/3 UDP and bootstrap TCP listen address")
	fs.BoolVar(&cfg.EnableH2, "enable-h2", cfg.EnableH2, "enable native HTTP/2")
	fs.BoolVar(&cfg.EnableH3, "enable-h3", cfg.EnableH3, "enable native HTTP/3")
	fs.StringVar(&cfg.TLSCert, "tls-cert", cfg.TLSCert, "TLS certificate PEM path")
	fs.StringVar(&cfg.TLSKey, "tls-key", cfg.TLSKey, "TLS private key PEM path")
	fs.StringVar(&cfg.PublicH1Origin, "public-h1-origin", cfg.PublicH1Origin, "public HTTP/1.1 origin")
	fs.StringVar(&cfg.PublicH2Origin, "public-h2-origin", cfg.PublicH2Origin, "public HTTP/2 origin")
	fs.StringVar(&cfg.PublicH3Origin, "public-h3-origin", cfg.PublicH3Origin, "public HTTP/3 origin")
	fs.StringVar(&cfg.ServerName, "name", cfg.ServerName, "server name advertised in /preflight")
	fs.StringVar(&cfg.ServerLocation, "location", cfg.ServerLocation, "server location label")
	fs.BoolVar(&cfg.Verbose, "verbose", cfg.Verbose, "log per-second download/upload throughput")
}
