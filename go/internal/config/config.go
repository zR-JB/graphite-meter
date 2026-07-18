// Package config loads and validates server configuration.
package config

import (
	"fmt"
	"net/netip"
	"net/url"
	"os"
	"strconv"
	"strings"
	"time"

	"github.com/zR-JB/graphite-meter/go/internal/origin"
)

var EngineVersion = "0.0.0-dev"

const (
	NativeH1Clear = "http1-clear"
	NativeH1TLS   = "http1-tls"
	NativeH2      = "http2"
	NativeH3      = "http3"
)

type NativeListeners struct {
	H1, H1TLS, H2, H3 string
}

type NativeOrigins struct {
	H1, H1TLS, H2, H3 string
}

type PublicOrigins struct {
	Both, Throughput, Latency []string
}

type Config struct {
	Native                                    NativeListeners
	NativePublic                              NativeOrigins
	AdvertisedNative                          map[string]bool
	AdvertiseAllNative                        bool
	Public                                    PublicOrigins
	TLSCert, TLSKey                           string
	ServerName, ServerLocation, EngineVersion string
	Verbose                                   bool
	TrustedProxies                            []netip.Prefix
	MaxActiveMeasurements                     int
	MaxActiveMeasurementsPerClient            int
	MaxConnections                            int
	MaxConnectionsPerClient                   int
	MaxOperationDuration                      time.Duration
}

func Default() Config {
	return Config{
		Native:           NativeListeners{H1: ":7246"},
		AdvertisedNative: map[string]bool{}, AdvertiseAllNative: true,
		ServerName: "graphite-meter", EngineVersion: EngineVersion,
		MaxActiveMeasurements: 256, MaxActiveMeasurementsPerClient: 32,
		MaxConnections: 512, MaxConnectionsPerClient: 64,
		MaxOperationDuration: 5 * time.Minute,
	}
}

func Load() (Config, error) {
	c := Default()
	for _, e := range []struct {
		name string
		dst  *string
	}{
		{"GM_H1_ADDR", &c.Native.H1}, {"GM_H1_TLS_ADDR", &c.Native.H1TLS}, {"GM_H2_ADDR", &c.Native.H2}, {"GM_H3_ADDR", &c.Native.H3},
		{"GM_TLS_CERT", &c.TLSCert}, {"GM_TLS_KEY", &c.TLSKey},
		{"GM_H1_PUBLIC_ORIGIN", &c.NativePublic.H1}, {"GM_H1_TLS_PUBLIC_ORIGIN", &c.NativePublic.H1TLS}, {"GM_H2_PUBLIC_ORIGIN", &c.NativePublic.H2}, {"GM_H3_PUBLIC_ORIGIN", &c.NativePublic.H3},
		{"GM_SERVER_NAME", &c.ServerName}, {"GM_SERVER_LOCATION", &c.ServerLocation},
	} {
		if v, ok := os.LookupEnv(e.name); ok {
			*e.dst = strings.TrimSpace(v)
		}
	}
	if v, ok := os.LookupEnv("GM_ADVERTISED_NATIVE_ENDPOINTS"); ok {
		set, err := ParseAdvertisedNative(v)
		if err != nil {
			return Config{}, fmt.Errorf("GM_ADVERTISED_NATIVE_ENDPOINTS: %w", err)
		}
		c.AdvertisedNative = set
		c.AdvertiseAllNative = strings.TrimSpace(v) == "all"
	}
	for _, e := range []struct {
		name string
		dst  *[]string
	}{
		{"GM_PUBLIC_ORIGINS", &c.Public.Both}, {"GM_PUBLIC_THROUGHPUT_ORIGINS", &c.Public.Throughput}, {"GM_PUBLIC_LATENCY_ORIGINS", &c.Public.Latency},
	} {
		if v, ok := os.LookupEnv(e.name); ok {
			*e.dst = splitList(v)
		}
	}
	var err error
	if c.Verbose, err = envBool("GM_VERBOSE", false); err != nil {
		return Config{}, err
	}
	for _, e := range []struct {
		name string
		dst  *int
	}{
		{"GM_MAX_ACTIVE_MEASUREMENTS", &c.MaxActiveMeasurements}, {"GM_MAX_ACTIVE_MEASUREMENTS_PER_CLIENT", &c.MaxActiveMeasurementsPerClient},
		{"GM_MAX_CONNECTIONS", &c.MaxConnections}, {"GM_MAX_CONNECTIONS_PER_CLIENT", &c.MaxConnectionsPerClient},
	} {
		if *e.dst, err = envInt(e.name, *e.dst); err != nil {
			return Config{}, err
		}
	}
	if v := os.Getenv("GM_MAX_OPERATION_DURATION"); v != "" {
		if c.MaxOperationDuration, err = time.ParseDuration(v); err != nil {
			return Config{}, fmt.Errorf("GM_MAX_OPERATION_DURATION: %w", err)
		}
	}
	if v := os.Getenv("GM_TRUSTED_PROXIES"); v != "" {
		for _, raw := range strings.Split(v, ",") {
			prefix, err := netip.ParsePrefix(strings.TrimSpace(raw))
			if err != nil {
				return Config{}, fmt.Errorf("GM_TRUSTED_PROXIES: %q: %w", raw, err)
			}
			c.TrustedProxies = append(c.TrustedProxies, prefix)
		}
	}
	return c, nil
}

func splitList(raw string) []string {
	var out []string
	for _, value := range strings.Split(raw, ",") {
		if value = strings.TrimSpace(value); value != "" {
			out = append(out, value)
		}
	}
	return out
}

func ParseAdvertisedNative(raw string) (map[string]bool, error) {
	set := map[string]bool{}
	switch strings.TrimSpace(raw) {
	case "", "none":
		return set, nil
	case "all":
		for _, v := range []string{NativeH1Clear, NativeH1TLS, NativeH2, NativeH3} {
			set[v] = true
		}
		return set, nil
	}
	for _, value := range splitList(raw) {
		switch value {
		case NativeH1Clear, NativeH1TLS, NativeH2, NativeH3:
			set[value] = true
		default:
			return nil, fmt.Errorf("unknown endpoint %q", value)
		}
	}
	return set, nil
}

func (c Config) NativeEnabled(name string) bool {
	switch name {
	case NativeH1Clear:
		return c.Native.H1 != ""
	case NativeH1TLS:
		return c.Native.H1TLS != ""
	case NativeH2:
		return c.Native.H2 != ""
	case NativeH3:
		return c.Native.H3 != ""
	}
	return false
}

func (c Config) NativeAdvertised(name string) bool {
	return c.NativeEnabled(name) && (c.AdvertiseAllNative || c.AdvertisedNative[name])
}

func envBool(name string, fallback bool) (bool, error) {
	v, ok := os.LookupEnv(name)
	if !ok {
		return fallback, nil
	}
	switch strings.ToLower(strings.TrimSpace(v)) {
	case "1", "true":
		return true, nil
	case "0", "false":
		return false, nil
	default:
		return false, fmt.Errorf("%s must be true/false or 1/0", name)
	}
}

func envInt(name string, fallback int) (int, error) {
	v, ok := os.LookupEnv(name)
	if !ok {
		return fallback, nil
	}
	n, err := strconv.Atoi(strings.TrimSpace(v))
	if err != nil {
		return 0, fmt.Errorf("%s must be an integer", name)
	}
	return n, nil
}

func validOrigin(value, scheme string, self bool) bool {
	if self && value == "self" {
		return true
	}
	u, err := url.Parse(value)
	return err == nil && (scheme == "" || u.Scheme == scheme) && (u.Scheme == "http" || u.Scheme == "https") && u.Hostname() != "" && u.Path == "" && u.User == nil && u.RawQuery == "" && !u.ForceQuery && u.Fragment == ""
}

func (c Config) Validate() error {
	for _, v := range []struct {
		name  string
		value int
	}{{"GM_MAX_ACTIVE_MEASUREMENTS", c.MaxActiveMeasurements}, {"GM_MAX_ACTIVE_MEASUREMENTS_PER_CLIENT", c.MaxActiveMeasurementsPerClient}, {"GM_MAX_CONNECTIONS", c.MaxConnections}, {"GM_MAX_CONNECTIONS_PER_CLIENT", c.MaxConnectionsPerClient}} {
		if v.value <= 0 {
			return fmt.Errorf("%s must be greater than zero", v.name)
		}
	}
	if c.MaxActiveMeasurementsPerClient > c.MaxActiveMeasurements {
		return fmt.Errorf("GM_MAX_ACTIVE_MEASUREMENTS_PER_CLIENT must not exceed GM_MAX_ACTIVE_MEASUREMENTS")
	}
	if c.MaxConnectionsPerClient > c.MaxConnections {
		return fmt.Errorf("GM_MAX_CONNECTIONS_PER_CLIENT must not exceed GM_MAX_CONNECTIONS")
	}
	if c.MaxOperationDuration <= 0 {
		return fmt.Errorf("GM_MAX_OPERATION_DURATION must be greater than zero")
	}
	if c.Native.H1 == "" {
		return fmt.Errorf("GM_H1_ADDR must not be empty")
	}
	if c.Native.H1TLS != "" || c.Native.H2 != "" || c.Native.H3 != "" {
		if c.TLSCert == "" || c.TLSKey == "" {
			return fmt.Errorf("GM_TLS_CERT and GM_TLS_KEY are required when a native TLS listener is enabled")
		}
	}
	listeners := []struct{ name, addr string }{{"GM_H1_ADDR", c.Native.H1}, {"GM_H1_TLS_ADDR", c.Native.H1TLS}, {"GM_H2_ADDR", c.Native.H2}, {"GM_H3_ADDR", c.Native.H3}}
	for i, a := range listeners {
		for _, b := range listeners[i+1:] {
			if a.addr != "" && a.addr == b.addr {
				return fmt.Errorf("%s and %s must differ", a.name, b.name)
			}
		}
	}
	if !c.AdvertiseAllNative {
		for name := range c.AdvertisedNative {
			if !c.NativeEnabled(name) {
				return fmt.Errorf("GM_ADVERTISED_NATIVE_ENDPOINTS includes disabled endpoint %q", name)
			}
		}
	}
	for _, v := range []struct{ name, value, scheme string }{{"GM_H1_PUBLIC_ORIGIN", c.NativePublic.H1, "http"}, {"GM_H1_TLS_PUBLIC_ORIGIN", c.NativePublic.H1TLS, "https"}, {"GM_H2_PUBLIC_ORIGIN", c.NativePublic.H2, "https"}, {"GM_H3_PUBLIC_ORIGIN", c.NativePublic.H3, "https"}} {
		if v.value != "" && !validOrigin(v.value, v.scheme, false) {
			return fmt.Errorf("%s must be an origin with %s scheme", v.name, v.scheme)
		}
	}
	for _, v := range []struct {
		name   string
		values []string
	}{{"GM_PUBLIC_ORIGINS", c.Public.Both}, {"GM_PUBLIC_THROUGHPUT_ORIGINS", c.Public.Throughput}, {"GM_PUBLIC_LATENCY_ORIGINS", c.Public.Latency}} {
		for _, value := range v.values {
			if !validOrigin(value, "", true) {
				return fmt.Errorf("%s contains invalid origin %q", v.name, value)
			}
		}
	}
	fixed := map[string]string{}
	for _, endpoint := range []struct{ name, origin, protocol string }{{NativeH1Clear, c.NativePublic.H1, "http1"}, {NativeH1TLS, c.NativePublic.H1TLS, "http1"}, {NativeH2, c.NativePublic.H2, "http2"}, {NativeH3, c.NativePublic.H3, "http3"}} {
		if endpoint.origin == "" || !c.NativeAdvertised(endpoint.name) {
			continue
		}
		key := origin.Key(endpoint.origin)
		if protocol, ok := fixed[key]; ok && protocol != endpoint.protocol {
			return fmt.Errorf("native origin %q is advertised with multiple deterministic protocols", endpoint.origin)
		}
		fixed[key] = endpoint.protocol
	}
	for _, origins := range [][]string{c.Public.Both, c.Public.Throughput} {
		for _, publicOrigin := range origins {
			if _, ok := fixed[origin.Key(publicOrigin)]; ok {
				return fmt.Errorf("origin %q cannot be both native deterministic and public negotiated", publicOrigin)
			}
		}
	}
	if !c.NativeAdvertised(NativeH1Clear) && !c.NativeAdvertised(NativeH1TLS) && !c.NativeAdvertised(NativeH2) && !c.NativeAdvertised(NativeH3) && len(c.Public.Both) == 0 && len(c.Public.Throughput) == 0 {
		return fmt.Errorf("configuration advertises no throughput endpoint")
	}
	return nil
}
