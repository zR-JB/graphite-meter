// Package config loads server configuration from flags + environment.
//
// Defaults favor the "just works" path: plain HTTP/1.1 on :8080, no TLS, no
// HTTP/3 / Alt-Svc. The TLS/h3 fields are reserved for Stage 5; AdvertiseH3
// stays false so an h1.1 throughput test is never auto-migrated onto QUIC.
package config

import (
	"os"
)

// EngineVersion is overridable at build time via
//
//	-ldflags="-X github.com/zR-JB/graphite-meter/go/internal/config.EngineVersion=1.2.3"
var EngineVersion = "0.1.0-dev"

// Config is the resolved server configuration.
type Config struct {
	// HTTP/1.1 cleartext listen address (default, always on).
	H1Addr string

	// Reserved for Stage 5 (HTTP/3 + WebTransport over TLS). Unused today.
	H3Addr      string
	TLSCert     string
	TLSKey      string
	AdvertiseH3 bool

	// Externally-reachable origins advertised in /preflight. Empty => derive
	// from the request Host (correct for direct access; set these behind a
	// reverse proxy so the client targets the right public URLs).
	PublicH1Origin  string
	PublicTLSOrigin string
	PublicH3Origin  string

	// Server identity surfaced in /preflight.
	ServerName     string
	ServerLocation string

	// Build/engine version surfaced in /preflight.
	EngineVersion string

	// Verbose enables per-second throughput logging on the download/upload
	// endpoints (the server-side counterpart to the client's debug logging), so
	// server-sent / drained rates can be compared against the kernel interface
	// counters and the browser's own figures. Off by default.
	Verbose bool
}

// Default returns a Config with the baseline defaults.
func Default() Config {
	return Config{
		H1Addr:        ":8080",
		H3Addr:        ":8443",
		AdvertiseH3:   false,
		ServerName:    "graphite-meter",
		EngineVersion: EngineVersion,
	}
}

// Load builds a Config from the environment, overlaid on Default().
// Flags (parsed by the caller) take final precedence.
func Load() Config {
	c := Default()
	if v := os.Getenv("GM_H1_ADDR"); v != "" {
		c.H1Addr = v
	}
	if v := os.Getenv("GM_H3_ADDR"); v != "" {
		c.H3Addr = v
	}
	if v := os.Getenv("GM_TLS_CERT"); v != "" {
		c.TLSCert = v
	}
	if v := os.Getenv("GM_TLS_KEY"); v != "" {
		c.TLSKey = v
	}
	if v := os.Getenv("GM_ADVERTISE_H3"); v == "1" || v == "true" {
		c.AdvertiseH3 = true
	}
	if v := os.Getenv("PUBLIC_H1_ORIGIN"); v != "" {
		c.PublicH1Origin = v
	}
	if v := os.Getenv("PUBLIC_TLS_ORIGIN"); v != "" {
		c.PublicTLSOrigin = v
	}
	if v := os.Getenv("PUBLIC_H3_ORIGIN"); v != "" {
		c.PublicH3Origin = v
	}
	if v := os.Getenv("GM_SERVER_NAME"); v != "" {
		c.ServerName = v
	}
	if v := os.Getenv("GM_SERVER_LOCATION"); v != "" {
		c.ServerLocation = v
	}
	if v := os.Getenv("GM_VERBOSE"); v == "1" || v == "true" {
		c.Verbose = true
	}
	return c
}
