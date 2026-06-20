// Command graphite-meter is the measurement server. Stage 1: serves the
// embedded Svelte client and GET /preflight over HTTP/1.1.
package main

import (
	"context"
	"flag"
	"log"
	"os/signal"
	"syscall"

	"github.com/zR-JB/graphite-meter/server/internal/config"
	"github.com/zR-JB/graphite-meter/server/internal/server"
)

func main() {
	cfg := config.Load()

	// Flags take final precedence over env/defaults.
	flag.StringVar(&cfg.H1Addr, "addr", cfg.H1Addr, "HTTP/1.1 listen address")
	flag.StringVar(&cfg.ServerName, "name", cfg.ServerName, "server name advertised in /preflight")
	flag.StringVar(&cfg.ServerLocation, "location", cfg.ServerLocation, "server location label")
	flag.BoolVar(&cfg.Verbose, "verbose", cfg.Verbose, "log per-second download/upload throughput")
	flag.Parse()

	ctx, stop := signal.NotifyContext(context.Background(), syscall.SIGINT, syscall.SIGTERM)
	defer stop()

	if err := server.Run(ctx, &cfg); err != nil {
		log.Fatalf("server error: %v", err)
	}
}
