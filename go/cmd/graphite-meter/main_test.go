package main

import (
	"flag"
	"testing"

	"github.com/zR-JB/graphite-meter/go/internal/config"
)

func TestH1AddressFlags(t *testing.T) {
	for _, tc := range []struct {
		name string
		args []string
		want string
	}{
		{"legacy addr", []string{"-addr", ":9000"}, ":9000"},
		{"h1 addr", []string{"-h1-addr", ":9001"}, ":9001"},
		{"last occurrence wins", []string{"-addr", ":9000", "-h1-addr", ":9001"}, ":9001"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			cfg := config.Default()
			fs := flag.NewFlagSet("test", flag.ContinueOnError)
			registerFlags(fs, &cfg)
			if err := fs.Parse(tc.args); err != nil {
				t.Fatal(err)
			}
			if cfg.H1Addr != tc.want {
				t.Fatalf("H1Addr = %q, want %q", cfg.H1Addr, tc.want)
			}
		})
	}
}
