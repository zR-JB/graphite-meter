// Package server wires the registry + static handler into the shared mux and
// runs the listener. Only HTTP/1.1 is served today; TLS/h3 (WebTransport) are
// reserved for a later stage — see docs/ARCHITECTURE.md#roadmap.
package server

import (
	"context"
	"errors"
	"log"
	"net"
	"net/http"
	"time"

	"github.com/zR-JB/graphite-meter/go/internal/config"
	"github.com/zR-JB/graphite-meter/go/internal/endpoint"
	"github.com/zR-JB/graphite-meter/go/internal/rng"
	"github.com/zR-JB/graphite-meter/go/internal/static"
)

// BuildMux constructs the shared mux: registered endpoints + the static client
// at "/". ctx is the server's run context, bounding every WebSocket bus
// handler's lifetime.
func BuildMux(ctx context.Context, reg *endpoint.Registry) *http.ServeMux {
	mux := http.NewServeMux()
	reg.Mount(ctx, mux)
	mux.Handle("/", static.Handler())
	return mux
}

// Run starts the server and blocks until ctx is cancelled, then shuts down
// gracefully.
func Run(ctx context.Context, cfg *config.Config) error {
	// One shared immutable RNG block, generated once: every download serves
	// slices of it, never regenerating per request.
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

	// uploadStore correlates an upload's POST lanes with its /ws/upload progress
	// socket (same minted ?id=, separate connections) into one authoritative
	// drained-byte count; its sweeper reaps idle test state.
	uploadStore := endpoint.NewUploadStore()
	go uploadStore.RunSweeper(ctx)

	reg := endpoint.NewRegistry()
	reg.RegisterHTTP("/preflight", endpoint.NewPreflight(cfg))
	reg.RegisterHTTP("/download", endpoint.NewDownload(block, dlMeter))
	reg.RegisterHTTP("/upload/session", endpoint.NewUploadSession(uploadStore))
	reg.RegisterHTTP("/upload", endpoint.NewUpload(ulMeter, uploadStore))
	reg.RegisterWS("/ws/ping", endpoint.NewPing())
	reg.RegisterWS("/ws/upload", endpoint.NewUploadProgress(uploadStore))

	mux := BuildMux(ctx, reg)

	srv := &http.Server{
		Handler:           mux,
		ReadHeaderTimeout: 10 * time.Second,
		// Go already defaults NoDelay=true, but a /ws/ping latency frame must
		// never sit in Nagle's buffer, so make TCP_NODELAY explicit rather than
		// relying on the default.
		ConnContext: func(ctx context.Context, c net.Conn) context.Context {
			if tc, ok := c.(*net.TCPConn); ok {
				_ = tc.SetNoDelay(true)
			}
			return ctx
		},
	}

	ln, err := net.Listen("tcp", cfg.H1Addr)
	if err != nil {
		return err
	}

	// Graceful shutdown on ctx cancel.
	go func() {
		<-ctx.Done()
		shutdownCtx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
		defer cancel()
		_ = srv.Shutdown(shutdownCtx)
	}()

	log.Printf("graphite-meter %s listening on %s (http/1.1)", cfg.EngineVersion, cfg.H1Addr)
	return serve(ln, srv)
}

// serve runs srv over an already-created listener, so tests can drive it
// against an ephemeral port. Mirrors ListenAndServe's error handling: a
// listener close is the expected shutdown path, not a failure.
func serve(ln net.Listener, srv *http.Server) error {
	if err := srv.Serve(ln); err != nil && !errors.Is(err, http.ErrServerClosed) {
		return err
	}
	return nil
}
