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
)

var EngineVersion = "0.0.0-dev"

type Config struct {
	H1Addr, H1TLSAddr, H2Addr, H3Addr         string
	EnableH1TLS, EnableH2, EnableH3           bool
	TLSCert, TLSKey                           string
	PublicH1Origin, PublicH1TLSOrigin         string
	PublicH2Origin, PublicH3Origin            string
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
		H1Addr: ":7246", H1TLSAddr: ":7247", H2Addr: ":7248", H3Addr: ":7249",
		ServerName: "graphite-meter", EngineVersion: EngineVersion,
		MaxActiveMeasurements: 256, MaxActiveMeasurementsPerClient: 32,
		MaxConnections: 512, MaxConnectionsPerClient: 64,
		MaxOperationDuration: 5 * time.Minute,
	}
}

func Load() (Config, error) {
	c := Default()
	if v := os.Getenv("PUBLIC_TLS_ORIGIN"); v != "" {
		c.PublicH2Origin = v
	}
	stringEnv := []struct {
		name string
		dst  *string
	}{
		{"GM_H1_ADDR", &c.H1Addr}, {"GM_H1_TLS_ADDR", &c.H1TLSAddr}, {"GM_H2_ADDR", &c.H2Addr}, {"GM_H3_ADDR", &c.H3Addr},
		{"GM_TLS_CERT", &c.TLSCert}, {"GM_TLS_KEY", &c.TLSKey},
		{"PUBLIC_H1_ORIGIN", &c.PublicH1Origin}, {"PUBLIC_H1_TLS_ORIGIN", &c.PublicH1TLSOrigin}, {"PUBLIC_H2_ORIGIN", &c.PublicH2Origin}, {"PUBLIC_H3_ORIGIN", &c.PublicH3Origin},
		{"GM_SERVER_NAME", &c.ServerName}, {"GM_SERVER_LOCATION", &c.ServerLocation},
	}
	for _, e := range stringEnv {
		if v := os.Getenv(e.name); v != "" {
			*e.dst = v
		}
	}
	var err error
	if c.EnableH1TLS, err = envBool("GM_ENABLE_H1_TLS", false); err != nil {
		return Config{}, err
	}
	if c.EnableH2, err = envBool("GM_ENABLE_H2", false); err != nil {
		return Config{}, err
	}
	if c.EnableH3, err = envBool("GM_ENABLE_H3", false); err != nil {
		return Config{}, err
	}
	if c.Verbose, err = envBool("GM_VERBOSE", false); err != nil {
		return Config{}, err
	}
	for _, e := range []struct {
		name string
		dst  *int
	}{
		{"GM_MAX_ACTIVE_MEASUREMENTS", &c.MaxActiveMeasurements},
		{"GM_MAX_ACTIVE_MEASUREMENTS_PER_CLIENT", &c.MaxActiveMeasurementsPerClient},
		{"GM_MAX_CONNECTIONS", &c.MaxConnections},
		{"GM_MAX_CONNECTIONS_PER_CLIENT", &c.MaxConnectionsPerClient},
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

func (c Config) Validate() error {
	for _, v := range []struct {
		name  string
		value int
	}{
		{"GM_MAX_ACTIVE_MEASUREMENTS", c.MaxActiveMeasurements},
		{"GM_MAX_ACTIVE_MEASUREMENTS_PER_CLIENT", c.MaxActiveMeasurementsPerClient},
		{"GM_MAX_CONNECTIONS", c.MaxConnections},
		{"GM_MAX_CONNECTIONS_PER_CLIENT", c.MaxConnectionsPerClient},
	} {
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
	if c.H1Addr == "" {
		return fmt.Errorf("GM_H1_ADDR must not be empty")
	}
	if c.EnableH1TLS && c.H1TLSAddr == "" {
		return fmt.Errorf("GM_H1_TLS_ADDR must not be empty when HTTPS HTTP/1.1 is enabled")
	}
	if c.EnableH2 && c.H2Addr == "" {
		return fmt.Errorf("GM_H2_ADDR must not be empty when HTTP/2 is enabled")
	}
	if c.EnableH3 && c.H3Addr == "" {
		return fmt.Errorf("GM_H3_ADDR must not be empty when HTTP/3 is enabled")
	}
	if c.EnableH1TLS || c.EnableH2 || c.EnableH3 {
		if c.TLSCert == "" || c.TLSKey == "" {
			return fmt.Errorf("GM_TLS_CERT and GM_TLS_KEY are required when a native TLS listener is enabled")
		}
	}
	listeners := []struct {
		enabled    bool
		name, addr string
	}{{true, "GM_H1_ADDR", c.H1Addr}, {c.EnableH1TLS, "GM_H1_TLS_ADDR", c.H1TLSAddr}, {c.EnableH2, "GM_H2_ADDR", c.H2Addr}, {c.EnableH3, "GM_H3_ADDR", c.H3Addr}}
	for i, a := range listeners {
		for _, b := range listeners[i+1:] {
			if a.enabled && b.enabled && a.addr == b.addr {
				return fmt.Errorf("%s and %s must differ", a.name, b.name)
			}
		}
	}
	for _, v := range []struct{ name, value, scheme string }{
		{"PUBLIC_H1_ORIGIN", c.PublicH1Origin, "http"}, {"PUBLIC_H1_TLS_ORIGIN", c.PublicH1TLSOrigin, "https"}, {"PUBLIC_H2_ORIGIN", c.PublicH2Origin, "https"}, {"PUBLIC_H3_ORIGIN", c.PublicH3Origin, "https"},
	} {
		if v.value == "" {
			continue
		}
		u, err := url.Parse(v.value)
		if err != nil || u.Scheme != v.scheme || u.Hostname() == "" || u.Path != "" || u.User != nil || u.RawQuery != "" || u.ForceQuery || u.Fragment != "" {
			return fmt.Errorf("%s must be an origin with %s scheme", v.name, v.scheme)
		}
	}
	return nil
}
