package main

import (
	"flag"
	"io"
	"os"
	"strings"
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
	if cfg.ServerName != "edge-1" || cfg.MaxConnections != 128 || cfg.Native.H1 != "127.0.0.1:9100" {
		t.Fatalf("parseConfig applied flags wrong: %+v", cfg)
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
	w.Close()

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
	r, w, _ := os.Pipe()
	io.WriteString(w, "one\ntwo\n")
	w.Close()
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
	if len(cfg.Public.Both) != 2 || cfg.Public.Both[0] != "https://a.example" {
		t.Fatalf("public-origins = %v", cfg.Public.Both)
	}
	if len(cfg.Public.Throughput) != 1 || len(cfg.Public.Latency) != 1 {
		t.Fatalf("role origins = %+v", cfg.Public)
	}
	if len(cfg.Auth.OIDCAllowedGroups) != 2 || cfg.AdvertiseAllNative {
		t.Fatalf("groups/advertise = %v / %v", cfg.Auth.OIDCAllowedGroups, cfg.AdvertiseAllNative)
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
	got := splitFlagList(" a , ,b,  ,c ")
	if len(got) != 3 || got[0] != "a" || got[1] != "b" || got[2] != "c" {
		t.Fatalf("splitFlagList = %v", got)
	}
	if splitFlagList("") != nil {
		t.Fatalf("splitFlagList(\"\") = %v, want nil", splitFlagList(""))
	}
}
