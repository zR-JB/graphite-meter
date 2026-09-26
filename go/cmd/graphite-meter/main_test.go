package main

import (
	"errors"
	"flag"
	"io"
	"os"
	"slices"
	"strings"
	"testing"
	"time"

	"github.com/zR-JB/graphite-meter/go/internal/config"
)

func TestParseConfig(t *testing.T) {
	for _, tc := range []struct {
		name  string
		env   string // a GM_H2_ADDR the flags must complete
		args  []string
		check func(config.Config) bool // nil when parsing must fail
	}{
		{"admission", "", []string{
			"-max-active-measurements", "80", "-max-active-measurements-per-client", "20",
			"-max-active-sessions", "24", "-max-sessions-per-client", "3",
			"-max-connections", "160", "-max-connections-per-client", "40",
			"-max-operation-duration", "2m", "-max-session-duration", "3h",
		}, func(c config.Config) bool {
			return c.MaxActiveMeasurements == 80 && c.MaxActiveMeasurementsPerClient == 20 &&
				c.MaxActiveSessions == 24 && c.MaxSessionsPerClient == 3 &&
				c.MaxConnections == 160 && c.MaxConnectionsPerClient == 40 &&
				c.MaxOperationDuration == 2*time.Minute && c.MaxSessionDuration == 3*time.Hour
		}},
		{"identity and listener", "", []string{
			"-name", "edge-1", "-h1-addr", "127.0.0.1:9100", "-result-history-default",
		}, func(c config.Config) bool {
			return c.ServerName == "edge-1" && c.Native.H1 == "127.0.0.1:9100" && c.ResultHistoryDefault
		}},
		{"lists and origins", "", []string{
			"-advertised-native-endpoints", "none",
			"-public-origins", "https://a.example, https://b.example",
			"-public-throughput-origins", "https://dl.example",
			"-public-latency-origins", "https://ping.example",
		}, func(c config.Config) bool {
			return slices.Equal(c.Public.Both, []string{"https://a.example", "https://b.example"}) &&
				slices.Equal(c.Public.Throughput, []string{"https://dl.example"}) &&
				slices.Equal(c.Public.Latency, []string{"https://ping.example"}) &&
				c.AdvertisedNative != nil && len(c.AdvertisedNative) == 0
		}},
		{"flags complete the environment", ":7248", []string{"-tls-cert", "/cert.pem", "-tls-key", "/key.pem"},
			func(c config.Config) bool { return c.Native.H2 == ":7248" && c.TLSCert == "/cert.pem" }},
		{"unknown flag", "", []string{"-not-a-flag"}, nil},
		{"invalid configuration", "", []string{"-max-connections", "-5"}, nil},
		{"unknown native endpoint", "", []string{"-advertised-native-endpoints", "nonsense"}, nil},
		{"explicit default provider while off", "", []string{"-auth-oidc-provider-name", "Authelia"}, nil},
		{"explicit empty public URL while off", "", []string{"-auth-public-url="}, nil},
		{"explicit empty groups while off", "", []string{"-auth-oidc-allowed-groups="}, nil},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Setenv("GM_H2_ADDR", tc.env)
			c, err := parseConfig("test", tc.args, io.Discard)
			if tc.check == nil {
				if err == nil {
					t.Fatal("configuration accepted")
				}
				return
			}
			if err != nil {
				t.Fatal(err)
			}
			if !tc.check(c) {
				t.Fatalf("parsed %+v", c)
			}
		})
	}
	if _, err := parseConfig("test", []string{"-h"}, io.Discard); !errors.Is(err, flag.ErrHelp) {
		t.Fatalf("parseConfig(-h) = %v, want flag.ErrHelp", err)
	}
}

func TestHashPasswordOverPipes(t *testing.T) {
	for _, tc := range []struct{ input, want string }{
		{"correct horse\ncorrect horse\n", "$argon2id$"},
		{"one\ntwo\n", ""},
	} {
		r, w, err := os.Pipe()
		if err != nil {
			t.Fatal(err)
		}
		if _, err := io.WriteString(w, tc.input); err != nil {
			t.Fatal(err)
		}
		_ = w.Close()
		var out, prompts strings.Builder
		err = hashPassword(r, &out, &prompts)
		if tc.want == "" {
			if err == nil {
				t.Fatal("hashPassword accepted mismatched entries")
			}
			continue
		}
		if err != nil || !strings.HasPrefix(out.String(), tc.want) || !strings.Contains(prompts.String(), "Password:") {
			t.Fatalf("hashPassword = %q, prompts %q, %v", out.String(), prompts.String(), err)
		}
	}
}
