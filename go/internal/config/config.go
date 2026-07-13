// Package config loads and validates server configuration.
package config

import (
	"fmt"
	"net/netip"
	"net/url"
	"os"
	"strings"
)

var EngineVersion = "0.0.0-dev"

type Config struct {
	H1Addr, H2Addr, H3Addr                         string
	EnableH2, EnableH3                             bool
	TLSCert, TLSKey                                string
	PublicH1Origin, PublicH2Origin, PublicH3Origin string
	ServerName, ServerLocation, EngineVersion      string
	Verbose                                        bool
	TrustedProxies                                 []netip.Prefix
}

func Default() Config {
	return Config{H1Addr: ":8765", H2Addr: ":8443", H3Addr: ":8444", ServerName: "graphite-meter", EngineVersion: EngineVersion}
}

func Load() (Config, error) {
	c := Default()
	stringEnv := []struct {
		name string
		dst  *string
	}{
		{"GM_H1_ADDR", &c.H1Addr}, {"GM_H2_ADDR", &c.H2Addr}, {"GM_H3_ADDR", &c.H3Addr},
		{"GM_TLS_CERT", &c.TLSCert}, {"GM_TLS_KEY", &c.TLSKey},
		{"PUBLIC_H1_ORIGIN", &c.PublicH1Origin}, {"PUBLIC_H2_ORIGIN", &c.PublicH2Origin}, {"PUBLIC_H3_ORIGIN", &c.PublicH3Origin},
		{"GM_SERVER_NAME", &c.ServerName}, {"GM_SERVER_LOCATION", &c.ServerLocation},
	}
	for _, e := range stringEnv {
		if v := os.Getenv(e.name); v != "" {
			*e.dst = v
		}
	}
	var err error
	if c.EnableH2, err = envBool("GM_ENABLE_H2", false); err != nil {
		return Config{}, err
	}
	if c.EnableH3, err = envBool("GM_ENABLE_H3", false); err != nil {
		return Config{}, err
	}
	if c.Verbose, err = envBool("GM_VERBOSE", false); err != nil {
		return Config{}, err
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
	return c, c.Validate()
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

func (c Config) Validate() error {
	if c.H1Addr == "" {
		return fmt.Errorf("GM_H1_ADDR must not be empty")
	}
	if c.EnableH2 && c.H2Addr == "" {
		return fmt.Errorf("GM_H2_ADDR must not be empty when HTTP/2 is enabled")
	}
	if c.EnableH3 && c.H3Addr == "" {
		return fmt.Errorf("GM_H3_ADDR must not be empty when HTTP/3 is enabled")
	}
	if c.EnableH2 || c.EnableH3 {
		if c.TLSCert == "" || c.TLSKey == "" {
			return fmt.Errorf("GM_TLS_CERT and GM_TLS_KEY are required when HTTP/2 or HTTP/3 is enabled")
		}
	}
	if c.EnableH2 && c.EnableH3 && c.H2Addr == c.H3Addr {
		return fmt.Errorf("GM_H2_ADDR and GM_H3_ADDR must differ because HTTP/3 also binds TCP")
	}
	for _, v := range []struct{ name, value, scheme string }{
		{"PUBLIC_H1_ORIGIN", c.PublicH1Origin, "http"}, {"PUBLIC_H2_ORIGIN", c.PublicH2Origin, "https"}, {"PUBLIC_H3_ORIGIN", c.PublicH3Origin, "https"},
	} {
		if v.value == "" {
			continue
		}
		u, err := url.Parse(v.value)
		if err != nil || u.Scheme != v.scheme || u.Host == "" || u.Path != "" {
			return fmt.Errorf("%s must be an origin with %s scheme", v.name, v.scheme)
		}
	}
	return nil
}
