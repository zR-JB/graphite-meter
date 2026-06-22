// Package server wires the registry + static handler into the shared mux and
// runs the listeners. Stage 1 runs only the HTTP/1.1 listener; the TLS/h3
// (WebTransport) listeners are added in Stage 5, gated by Config.AdvertiseH3.
package server

import (
	"context"
	"errors"
	"log"
	"net"
	"net/http"
	"time"

	"github.com/zR-JB/graphite-meter/server/internal/config"
	"github.com/zR-JB/graphite-meter/server/internal/endpoint"
	"github.com/zR-JB/graphite-meter/server/internal/rng"
	"github.com/zR-JB/graphite-meter/server/internal/static"
)

// BuildMux constructs the shared mux: registered endpoints + the static client
// at "/". The same mux is reused by every listener (h1/h2/h3) in later stages.
func BuildMux(reg *endpoint.Registry) *http.ServeMux {
	mux := http.NewServeMux()
	reg.Mount(mux)
	mux.Handle("/", static.Handler())
	return mux
}

// Run starts the server and blocks until ctx is cancelled, then shuts down
// gracefully.
func Run(ctx context.Context, cfg *config.Config) error {
	// One shared immutable RNG block, generated once: every download serves
	// slices of it, never regenerating per request (ARCHITECTURE §7).
	block := rng.NewBlock(rng.BlockSize)

	// Verbose mode: one per-direction throughput logger, each draining its own
	// 1 Hz goroutine for the run's lifetime. Nil when off — the endpoints'
	// meter calls are then cheap no-ops.
	var dlMeter, ulMeter *endpoint.Meter
	if cfg.Verbose {
		dlMeter = endpoint.NewMeter("server:download")
		ulMeter = endpoint.NewMeter("server:upload")
		go dlMeter.Run(ctx)
		go ulMeter.Run(ctx)
		log.Printf("[gm:server] verbose throughput logging enabled")
	}

	// Per-id upload aggregate store: correlates the upload's POST lanes with its
	// /ws/upload progress socket (separate connections, same minted ?id=) into one
	// server-authoritative drained-byte count. Its sweeper reaps idle test state.
	uploadStore := endpoint.NewUploadStore()
	go uploadStore.RunSweeper(ctx)

	reg := endpoint.NewRegistry()
	reg.RegisterHTTP("/preflight", endpoint.NewPreflight(cfg, uploadStore))
	reg.RegisterHTTP("/download", endpoint.NewDownload(block, dlMeter))
	reg.RegisterHTTP("/upload", endpoint.NewUpload(ulMeter))
	reg.RegisterWS("/ws/ping", endpoint.NewPing())

	mux := BuildMux(reg)

	srv := &http.Server{
		Addr:              cfg.H1Addr,
		Handler:           mux,
		ReadHeaderTimeout: 10 * time.Second,
		// TCP_NODELAY on every accepted conn (ARCHITECTURE §7): a single ping
		// frame on the /ws/ping latency bus must not sit in Nagle's buffer waiting
		// to coalesce. Go's net package already defaults NoDelay=true; we set it
		// explicitly because it is normative for accurate sub-ms latency.
		ConnContext: func(ctx context.Context, c net.Conn) context.Context {
			if tc, ok := c.(*net.TCPConn); ok {
				_ = tc.SetNoDelay(true)
			}
			return ctx
		},
	}

	// Graceful shutdown on ctx cancel.
	go func() {
		<-ctx.Done()
		shutdownCtx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
		defer cancel()
		_ = srv.Shutdown(shutdownCtx)
	}()

	log.Printf("graphite-meter %s listening on %s (http/1.1)", cfg.EngineVersion, cfg.H1Addr)
	if err := srv.ListenAndServe(); err != nil && !errors.Is(err, http.ErrServerClosed) {
		return err
	}
	return nil
}
