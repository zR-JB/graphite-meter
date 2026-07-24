package main

import (
	"flag"
	"io"
	"os"
	"slices"
	"strings"
	"testing"
	"time"

	"github.com/zR-JB/graphite-meter/go/internal/config"
)

func TestH1AddressFlagOverridesDefault(t *testing.T) {
	cfg := config.Default()
	fs := flag.NewFlagSet("test", flag.ContinueOnError)
	registerFlags(fs, &cfg)
	if err := fs.Parse([]string{"-h1-addr", ":9001"}); err != nil {
		t.Fatal(err)
	}
	if cfg.Native.H1 != ":9001" {
		t.Fatalf("Native.H1 = %q, want %q", cfg.Native.H1, ":9001")
	}
}

func TestAdmissionFlagsOverrideDefaults(t *testing.T) {
	cfg := config.Default()
	fs := flag.NewFlagSet("test", flag.ContinueOnError)
	registerFlags(fs, &cfg)
	err := fs.Parse([]string{"-max-active-measurements", "80", "-max-active-measurements-per-client", "20", "-max-connections", "160", "-max-connections-per-client", "40", "-max-operation-duration", "2m"})
	if err != nil {
		t.Fatal(err)
	}
	if cfg.MaxActiveMeasurements != 80 {
		t.Fatalf("MaxActiveMeasurements = %d, want 80", cfg.MaxActiveMeasurements)
	}
	if cfg.MaxActiveMeasurementsPerClient != 20 {
		t.Fatalf("MaxActiveMeasurementsPerClient = %d, want 20", cfg.MaxActiveMeasurementsPerClient)
	}
	if cfg.MaxConnections != 160 {
		t.Fatalf("MaxConnections = %d, want 160", cfg.MaxConnections)
	}
	if cfg.MaxConnectionsPerClient != 40 {
		t.Fatalf("MaxConnectionsPerClient = %d, want 40", cfg.MaxConnectionsPerClient)
	}
	if cfg.MaxOperationDuration != 2*time.Minute {
		t.Fatalf("MaxOperationDuration = %v, want %v", cfg.MaxOperationDuration, 2*time.Minute)
	}
}

func TestExplicitAuthFlagsAreRejectedWhileOff(t *testing.T) {
	for _, args := range [][]string{
		{"-auth-oidc-provider-name", "Authelia"},
		{"-auth-public-url="},
		{"-auth-oidc-allowed-groups="},
	} {
		cfg := config.Default()
		fs := flag.NewFlagSet("test", flag.ContinueOnError)
		registerFlags(fs, &cfg)
		if err := fs.Parse(args); err != nil {
			t.Fatal(err)
		}
		if err := cfg.Validate(); err == nil {
			t.Fatalf("explicit auth flags %q accepted while authentication is off", args)
		}
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

func TestParseConfigAppliesFlagsAndValidates(t *testing.T) {
	cfg, err := parseConfig("test", []string{"-name", "edge-1", "-max-connections", "128", "-h1-addr", "127.0.0.1:9100"})
	if err != nil {
		t.Fatalf("parseConfig: %v", err)
	}
	if cfg.ServerName != "edge-1" {
		t.Fatalf("ServerName = %q, want %q", cfg.ServerName, "edge-1")
	}
	if cfg.MaxConnections != 128 {
		t.Fatalf("MaxConnections = %d, want 128", cfg.MaxConnections)
	}
	if cfg.Native.H1 != "127.0.0.1:9100" {
		t.Fatalf("Native.H1 = %q, want %q", cfg.Native.H1, "127.0.0.1:9100")
	}
}

func TestParseConfigRejectsInvalidFlagAndConfig(t *testing.T) {
	if _, err := parseConfig("test", []string{"-not-a-flag"}); err == nil {
		t.Fatal("parseConfig accepted an unknown flag")
	}
	if _, err := parseConfig("test", []string{"-max-connections", "-5"}); err == nil {
		t.Fatal("parseConfig accepted a configuration that fails validation")
	}
}

func TestHashPasswordEmitsAPHCHashOverPipes(t *testing.T) {
	r, w, err := os.Pipe()
	if err != nil {
		t.Fatal(err)
	}
	if _, err := io.WriteString(w, "correct horse\ncorrect horse\n"); err != nil {
		t.Fatal(err)
	}
	if err := w.Close(); err != nil {
		t.Fatal(err)
	}

	var out, prompts strings.Builder
	if err := hashPassword(r, &out, &prompts); err != nil {
		t.Fatalf("hashPassword: %v", err)
	}
	if got := strings.TrimSpace(out.String()); !strings.HasPrefix(got, "$argon2id$") {
		t.Fatalf("hashPassword out = %q, want an argon2id PHC hash", got)
	}
	if !strings.Contains(prompts.String(), "Password:") {
		t.Fatalf("hashPassword did not prompt: %q", prompts.String())
	}
}

func TestHashPasswordRejectsMismatch(t *testing.T) {
	r, w, err := os.Pipe()
	if err != nil {
		t.Fatal(err)
	}
	if _, err := io.WriteString(w, "one\ntwo\n"); err != nil {
		t.Fatal(err)
	}
	if err := w.Close(); err != nil {
		t.Fatal(err)
	}
	if err := hashPassword(r, io.Discard, io.Discard); err == nil {
		t.Fatal("hashPassword accepted mismatched entries")
	}
}

func TestListAndOriginFlagsPopulateConfig(t *testing.T) {
	cfg := config.Default()
	fs := flag.NewFlagSet("test", flag.ContinueOnError)
	registerFlags(fs, &cfg)
	err := fs.Parse([]string{
		"-advertised-native-endpoints", "none",
		"-public-origins", "https://a.example, https://b.example",
		"-public-throughput-origins", "https://dl.example",
		"-public-latency-origins", "https://ping.example",
		"-auth-oidc-allowed-groups", "admins, ops",
	})
	if err != nil {
		t.Fatal(err)
	}
	wantBoth := []string{"https://a.example", "https://b.example"}
	if !slices.Equal(cfg.Public.Both, wantBoth) {
		t.Fatalf("Public.Both = %v, want %v", cfg.Public.Both, wantBoth)
	}
	if !slices.Equal(cfg.Public.Throughput, []string{"https://dl.example"}) {
		t.Fatalf("Public.Throughput = %v, want %v", cfg.Public.Throughput, []string{"https://dl.example"})
	}
	if !slices.Equal(cfg.Public.Latency, []string{"https://ping.example"}) {
		t.Fatalf("Public.Latency = %v, want %v", cfg.Public.Latency, []string{"https://ping.example"})
	}
	if !slices.Equal(cfg.Auth.OIDCAllowedGroups, []string{"admins", "ops"}) {
		t.Fatalf("Auth.OIDCAllowedGroups = %v, want %v", cfg.Auth.OIDCAllowedGroups, []string{"admins", "ops"})
	}
	if cfg.AdvertiseAllNative {
		t.Fatalf("AdvertiseAllNative = true, want false for %q", "none")
	}
}

func TestAdvertisedNativeEndpointsRejectsGarbage(t *testing.T) {
	cfg := config.Default()
	fs := flag.NewFlagSet("test", flag.ContinueOnError)
	registerFlags(fs, &cfg)
	if err := fs.Parse([]string{"-advertised-native-endpoints", "nonsense"}); err == nil {
		t.Fatal("accepted an invalid advertised-native-endpoints value")
	}
}

func TestSplitFlagListTrimsAndDropsEmpties(t *testing.T) {
	want := []string{"a", "b", "c"}
	if got := splitFlagList(" a , ,b,  ,c "); !slices.Equal(got, want) {
		t.Fatalf("splitFlagList = %v, want %v", got, want)
	}
	if got := splitFlagList(""); got != nil {
		t.Fatalf("splitFlagList(\"\") = %v, want nil", got)
	}
}
