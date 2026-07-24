// Command graphite-meter serves the embedded Svelte client on H1 and shared
// measurement endpoints on configured H1/H2/H3 listeners.
package main

import (
	"bufio"
	"context"
	"errors"
	"flag"
	"fmt"
	"io"
	"log"
	"os"
	"os/signal"
	"strings"
	"syscall"

	"github.com/zR-JB/graphite-meter/go/internal/auth"
	"github.com/zR-JB/graphite-meter/go/internal/config"
	"github.com/zR-JB/graphite-meter/go/internal/server"
)

func main() {
	if len(os.Args) == 2 && os.Args[1] == "hash-password" {
		if err := hashPassword(os.Stdin, os.Stdout, os.Stderr); err != nil {
			log.Fatalf("hash-password: %v", err)
		}
		return
	}
	cfg, err := parseConfig("graphite-meter", os.Args[1:])
	if err != nil {
		log.Fatalf("configuration error: %v", err)
	}

	ctx, stop := signal.NotifyContext(context.Background(), syscall.SIGINT, syscall.SIGTERM)
	defer stop()

	if err := server.Run(ctx, &cfg); err != nil {
		log.Fatalf("server error: %v", err)
	}
}

// parseConfig loads the base configuration, applies the command-line flags in
// args, and validates the result. Split from main so the flag surface and the
// validation wiring are testable without spawning a process.
func parseConfig(name string, args []string) (config.Config, error) {
	cfg, err := config.Load()
	if err != nil {
		return config.Config{}, err
	}
	fs := flag.NewFlagSet(name, flag.ContinueOnError)
	registerFlags(fs, &cfg)
	if err := fs.Parse(args); err != nil {
		return config.Config{}, err
	}
	if err := cfg.Validate(); err != nil {
		return config.Config{}, err
	}
	return cfg, nil
}

// hashPassword reads a password twice from stdin (echoing nothing when it is a
// terminal), confirms the two match, and writes the Argon2id PHC hash to out.
// prompts carries the interactive "Password:" cues. stdin, out, and prompts are
// injected so the flow is testable over pipes.
func hashPassword(stdin *os.File, out, prompts io.Writer) error {
	in := bufio.NewReader(stdin)
	fmt.Fprint(prompts, "Password: ")
	first, err := auth.ReadPassword(in, stdin)
	if err != nil {
		return err
	}
	fmt.Fprint(prompts, "Confirm password: ")
	second, err := auth.ReadPassword(in, stdin)
	if err != nil {
		return err
	}
	if first != second {
		return errors.New("passwords do not match")
	}
	encoded, err := auth.HashPassword(first)
	if err != nil {
		return err
	}
	fmt.Fprintln(out, encoded)
	return nil
}

func registerFlags(fs *flag.FlagSet, cfg *config.Config) {
	fs.StringVar(&cfg.Native.H1, "h1-addr", cfg.Native.H1, "clear HTTP/1.1 listen address")
	fs.StringVar(&cfg.Native.H1TLS, "h1-tls-addr", cfg.Native.H1TLS, "HTTPS HTTP/1.1 listen address; empty disables it")
	fs.StringVar(&cfg.Native.H2, "h2-addr", cfg.Native.H2, "HTTP/2 TLS listen address; empty disables it")
	fs.StringVar(&cfg.Native.H3, "h3-addr", cfg.Native.H3, "HTTP/3 UDP and bootstrap TCP listen address; empty disables it")
	fs.StringVar(&cfg.TLSCert, "tls-cert", cfg.TLSCert, "TLS certificate PEM path")
	fs.StringVar(&cfg.TLSKey, "tls-key", cfg.TLSKey, "TLS private key PEM path")
	fs.StringVar(&cfg.NativePublic.H1, "h1-public-origin", cfg.NativePublic.H1, "public origin of the native clear HTTP/1.1 listener")
	fs.StringVar(&cfg.NativePublic.H1TLS, "h1-tls-public-origin", cfg.NativePublic.H1TLS, "public origin of the native HTTPS HTTP/1.1 listener")
	fs.StringVar(&cfg.NativePublic.H2, "h2-public-origin", cfg.NativePublic.H2, "public origin of the native HTTP/2 listener")
	fs.StringVar(&cfg.NativePublic.H3, "h3-public-origin", cfg.NativePublic.H3, "public origin of the native HTTP/3 listener")
	fs.Func("advertised-native-endpoints", "all, none, or comma-separated native endpoint names", func(value string) error {
		set, err := config.ParseAdvertisedNative(value)
		if err != nil {
			return err
		}
		cfg.AdvertisedNative = set
		cfg.AdvertiseAllNative = strings.TrimSpace(value) == "all"
		return nil
	})
	fs.Func("public-origins", "comma-separated negotiated origins providing throughput and latency", func(value string) error { cfg.Public.Both = splitFlagList(value); return nil })
	fs.Func("public-throughput-origins", "comma-separated negotiated throughput origins", func(value string) error { cfg.Public.Throughput = splitFlagList(value); return nil })
	fs.Func("public-latency-origins", "comma-separated WebSocket latency origins", func(value string) error { cfg.Public.Latency = splitFlagList(value); return nil })
	fs.StringVar(&cfg.ServerName, "name", cfg.ServerName, "server name advertised in /preflight")
	fs.StringVar(&cfg.ServerLocation, "location", cfg.ServerLocation, "server location label")
	fs.BoolVar(&cfg.Verbose, "verbose", cfg.Verbose, "log per-second download/upload throughput")
	fs.IntVar(&cfg.MaxActiveMeasurements, "max-active-measurements", cfg.MaxActiveMeasurements, "maximum concurrent measurement handlers")
	fs.IntVar(&cfg.MaxActiveMeasurementsPerClient, "max-active-measurements-per-client", cfg.MaxActiveMeasurementsPerClient, "maximum concurrent measurement handlers per client")
	fs.IntVar(&cfg.MaxConnections, "max-connections", cfg.MaxConnections, "maximum concurrent TCP and QUIC connections")
	fs.IntVar(&cfg.MaxConnectionsPerClient, "max-connections-per-client", cfg.MaxConnectionsPerClient, "maximum concurrent connections per direct client")
	fs.DurationVar(&cfg.MaxOperationDuration, "max-operation-duration", cfg.MaxOperationDuration, "maximum measurement operation lifetime")
	fs.StringVar(&cfg.Auth.Mode, "auth-mode", cfg.Auth.Mode, "authentication mode: off, password, oidc, or hybrid")
	authStringFlag(fs, cfg, "auth-public-url", &cfg.Auth.PublicURL, "canonical HTTPS UI origin")
	authStringFlag(fs, cfg, "auth-password-hash-file", &cfg.Auth.PasswordHashFile, "file containing the operator Argon2id PHC hash")
	authStringFlag(fs, cfg, "auth-oidc-issuer", &cfg.Auth.OIDCIssuer, "OIDC issuer URL")
	authStringFlag(fs, cfg, "auth-oidc-client-id", &cfg.Auth.OIDCClientID, "OIDC client ID")
	authStringFlag(fs, cfg, "auth-oidc-client-secret-file", &cfg.Auth.OIDCSecretFile, "file containing the OIDC client secret")
	fs.Func("auth-oidc-allowed-groups", "comma-separated case-sensitive OIDC groups", func(value string) error {
		cfg.Auth.Explicit = true
		cfg.Auth.OIDCAllowedGroups = splitFlagList(value)
		return nil
	})
	authStringFlag(fs, cfg, "auth-oidc-provider-name", &cfg.Auth.OIDCProviderName, "OIDC provider label")
}

func authStringFlag(fs *flag.FlagSet, cfg *config.Config, name string, target *string, usage string) {
	fs.Func(name, usage, func(value string) error {
		cfg.Auth.Explicit = true
		*target = strings.TrimSpace(value)
		return nil
	})
}

func splitFlagList(value string) []string {
	var values []string
	for _, item := range strings.Split(value, ",") {
		if item = strings.TrimSpace(item); item != "" {
			values = append(values, item)
		}
	}
	return values
}
