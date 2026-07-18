package main

import (
	"flag"
	"testing"
	"time"

	"github.com/zR-JB/graphite-meter/go/internal/config"
)

func TestH1AddressFlag(t *testing.T) {
	for _, tc := range []struct {
		name string
		args []string
		want string
	}{
		{"h1 addr", []string{"-h1-addr", ":9001"}, ":9001"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			cfg := config.Default()
			fs := flag.NewFlagSet("test", flag.ContinueOnError)
			registerFlags(fs, &cfg)
			if err := fs.Parse(tc.args); err != nil {
				t.Fatal(err)
			}
			if cfg.Native.H1 != tc.want {
				t.Fatalf("H1Addr = %q, want %q", cfg.Native.H1, tc.want)
			}
		})
	}
}

func TestAdmissionFlags(t *testing.T) {
	cfg := config.Default()
	fs := flag.NewFlagSet("test", flag.ContinueOnError)
	registerFlags(fs, &cfg)
	err := fs.Parse([]string{"-max-active-measurements", "80", "-max-active-measurements-per-client", "20", "-max-connections", "160", "-max-connections-per-client", "40", "-max-operation-duration", "2m"})
	if err != nil {
		t.Fatal(err)
	}
	if cfg.MaxActiveMeasurements != 80 || cfg.MaxActiveMeasurementsPerClient != 20 || cfg.MaxConnections != 160 || cfg.MaxConnectionsPerClient != 40 || cfg.MaxOperationDuration != 2*time.Minute {
		t.Fatalf("admission flags = %+v", cfg)
	}
}

func TestFlagsCompleteEnvironmentConfig(t *testing.T) {
	t.Setenv("GM_H2_ADDR", ":7248")
	cfg, err := config.Load()
	if err != nil {
		t.Fatal(err)
	}
	fs := flag.NewFlagSet("test", flag.ContinueOnError)
	registerFlags(fs, &cfg)
	if err := fs.Parse([]string{"-tls-cert", "/cert.pem", "-tls-key", "/key.pem"}); err != nil {
		t.Fatal(err)
	}
	if err := cfg.Validate(); err != nil {
		t.Fatalf("flags did not complete environment config: %v", err)
	}
}
