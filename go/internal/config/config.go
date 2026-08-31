// Package config loads and validates server configuration.
package config

import (
	"fmt"
	"maps"
	"net/netip"
	"net/url"
	"os"
	"strconv"
	"strings"
	"time"
	"unicode"

	"github.com/zR-JB/graphite-meter/go/internal/origin"
)

// EngineVersion is the server build version, stamped at link time.
var EngineVersion = "0.0.0-dev"

// Native endpoint names, as accepted by GM_ADVERTISED_NATIVE_ENDPOINTS and reported in preflight.
const (
	NativeH1Clear = "http1-clear"
	NativeH1TLS   = "http1-tls"
	NativeH2      = "http2"
	NativeH3      = "http3"
)

// NativeListeners holds the listen address of each native endpoint; an empty address disables that endpoint.
type NativeListeners struct {
	H1, H1TLS, H2, H3 string
}

// NativeOrigins holds the externally reachable origin advertised for each native listener.
type NativeOrigins struct {
	H1, H1TLS, H2, H3 string
}

// PublicOrigins holds proxied origins advertised for both measurement kinds, for throughput only, or for latency only.
type PublicOrigins struct {
	Both, Throughput, Latency []string
}

// AuthConfig holds the authentication settings read from GM_AUTH_*.
type AuthConfig struct {
	// Explicit is true when the operator sets any GM_AUTH_* variable other than the mode, even to its default value.
	Explicit          bool
	Mode              string
	PublicURL         string
	PasswordHash      string
	PasswordHashFile  string
	OIDCIssuer        string
	OIDCClientID      string
	OIDCClientSecret  string
	OIDCSecretFile    string
	OIDCAllowedGroups []string
	OIDCProviderName  string
}

// Config is the resolved server configuration.
type Config struct {
	Native                                    NativeListeners
	NativePublic                              NativeOrigins
	AdvertisedNative                          map[string]bool
	AdvertiseAllNative                        bool
	Public                                    PublicOrigins
	TLSCert, TLSKey                           string
	ServerName, ServerLocation, EngineVersion string
	// ResultHistoryDefault controls whether new browsers save completed results by default.
	ResultHistoryDefault           bool
	Verbose                        bool
	TrustedProxies                 []netip.Prefix
	MaxActiveMeasurements          int
	MaxActiveMeasurementsPerClient int
	MaxActiveSessions              int
	MaxSessionsPerClient           int
	MaxConnections                 int
	MaxConnectionsPerClient        int
	MaxOperationDuration           time.Duration
	MaxSessionDuration             time.Duration
	Auth                           AuthConfig
}

func Default() Config {
	return Config{
		Native:           NativeListeners{H1: ":7246"},
		AdvertisedNative: map[string]bool{}, AdvertiseAllNative: true,
		ServerName: "graphite-meter", EngineVersion: EngineVersion,
		MaxActiveMeasurements: 256, MaxActiveMeasurementsPerClient: 32,
		MaxActiveSessions: 64, MaxSessionsPerClient: 16,
		MaxConnections: 512, MaxConnectionsPerClient: 64,
		MaxOperationDuration: 5 * time.Minute,
		MaxSessionDuration:   2 * time.Hour,
		Auth:                 AuthConfig{Mode: "off", OIDCProviderName: "Authelia"},
	}
}

func Load() (Config, error) {
	c := Default()
	for _, env := range []struct {
		name string
		dst  *string
	}{
		{"GM_H1_ADDR", &c.Native.H1}, {"GM_H1_TLS_ADDR", &c.Native.H1TLS}, {"GM_H2_ADDR", &c.Native.H2}, {"GM_H3_ADDR", &c.Native.H3},
		{"GM_TLS_CERT", &c.TLSCert}, {"GM_TLS_KEY", &c.TLSKey},
		{"GM_H1_PUBLIC_ORIGIN", &c.NativePublic.H1}, {"GM_H1_TLS_PUBLIC_ORIGIN", &c.NativePublic.H1TLS}, {"GM_H2_PUBLIC_ORIGIN", &c.NativePublic.H2}, {"GM_H3_PUBLIC_ORIGIN", &c.NativePublic.H3},
		{"GM_SERVER_NAME", &c.ServerName}, {"GM_SERVER_LOCATION", &c.ServerLocation},
		{"GM_AUTH_MODE", &c.Auth.Mode}, {"GM_AUTH_PUBLIC_URL", &c.Auth.PublicURL},
		{"GM_AUTH_PASSWORD_HASH", &c.Auth.PasswordHash}, {"GM_AUTH_PASSWORD_HASH_FILE", &c.Auth.PasswordHashFile},
		{"GM_AUTH_OIDC_ISSUER", &c.Auth.OIDCIssuer}, {"GM_AUTH_OIDC_CLIENT_ID", &c.Auth.OIDCClientID},
		{"GM_AUTH_OIDC_CLIENT_SECRET", &c.Auth.OIDCClientSecret}, {"GM_AUTH_OIDC_CLIENT_SECRET_FILE", &c.Auth.OIDCSecretFile},
		{"GM_AUTH_OIDC_PROVIDER_NAME", &c.Auth.OIDCProviderName},
	} {
		if v, ok := os.LookupEnv(env.name); ok {
			*env.dst = strings.TrimSpace(v)
			if marksAuthExplicit(env.name) {
				c.Auth.Explicit = true
			}
		}
	}
	if v, ok := os.LookupEnv("GM_AUTH_OIDC_ALLOWED_GROUPS"); ok {
		c.Auth.Explicit = true
		c.Auth.OIDCAllowedGroups = splitList(v)
	}
	if v, ok := os.LookupEnv("GM_ADVERTISED_NATIVE_ENDPOINTS"); ok {
		set, err := ParseAdvertisedNative(v)
		if err != nil {
			return Config{}, fmt.Errorf("GM_ADVERTISED_NATIVE_ENDPOINTS: %w", err)
		}
		c.AdvertisedNative = set
		c.AdvertiseAllNative = strings.TrimSpace(v) == "all"
	}
	for _, env := range []struct {
		name string
		dst  *[]string
	}{
		{"GM_PUBLIC_ORIGINS", &c.Public.Both}, {"GM_PUBLIC_THROUGHPUT_ORIGINS", &c.Public.Throughput}, {"GM_PUBLIC_LATENCY_ORIGINS", &c.Public.Latency},
	} {
		if v, ok := os.LookupEnv(env.name); ok {
			*env.dst = splitList(v)
		}
	}
	var err error
	if c.Verbose, err = envBool("GM_VERBOSE", false); err != nil {
		return Config{}, err
	}
	if c.ResultHistoryDefault, err = envBool("GM_RESULT_HISTORY_DEFAULT", false); err != nil {
		return Config{}, err
	}
	for _, env := range []struct {
		name string
		dst  *int
	}{
		{"GM_MAX_ACTIVE_MEASUREMENTS", &c.MaxActiveMeasurements}, {"GM_MAX_ACTIVE_MEASUREMENTS_PER_CLIENT", &c.MaxActiveMeasurementsPerClient},
		{"GM_MAX_ACTIVE_SESSIONS", &c.MaxActiveSessions}, {"GM_MAX_SESSIONS_PER_CLIENT", &c.MaxSessionsPerClient},
		{"GM_MAX_CONNECTIONS", &c.MaxConnections}, {"GM_MAX_CONNECTIONS_PER_CLIENT", &c.MaxConnectionsPerClient},
	} {
		if *env.dst, err = envInt(env.name, *env.dst); err != nil {
			return Config{}, err
		}
	}
	for _, env := range []struct {
		name string
		dst  *time.Duration
	}{
		{"GM_MAX_OPERATION_DURATION", &c.MaxOperationDuration}, {"GM_MAX_SESSION_DURATION", &c.MaxSessionDuration},
	} {
		if v := os.Getenv(env.name); v != "" {
			if *env.dst, err = time.ParseDuration(v); err != nil {
				return Config{}, fmt.Errorf("%s: %w", env.name, err)
			}
		}
	}
	if v := os.Getenv("GM_TRUSTED_PROXIES"); v != "" {
		for raw := range strings.SplitSeq(v, ",") {
			prefix, err := netip.ParsePrefix(strings.TrimSpace(raw))
			if err != nil {
				return Config{}, fmt.Errorf("GM_TRUSTED_PROXIES: %q: %w", raw, err)
			}
			if isDefaultRoute(prefix) {
				return Config{}, fmt.Errorf("GM_TRUSTED_PROXIES: %q trusts every address; list the proxy's actual CIDR instead", raw)
			}
			c.TrustedProxies = append(c.TrustedProxies, prefix.Masked())
		}
	}
	return c, nil
}

func marksAuthExplicit(name string) bool {
	return strings.HasPrefix(name, "GM_AUTH_") && name != "GM_AUTH_MODE"
}

func isDefaultRoute(prefix netip.Prefix) bool {
	return prefix.Bits() == 0
}

func splitList(raw string) []string {
	var out []string
	for value := range strings.SplitSeq(raw, ",") {
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
		for _, name := range []string{NativeH1Clear, NativeH1TLS, NativeH2, NativeH3} {
			set[name] = true
		}
		return set, nil
	}
	for _, name := range splitList(raw) {
		switch name {
		case NativeH1Clear, NativeH1TLS, NativeH2, NativeH3:
			set[name] = true
		default:
			return nil, fmt.Errorf("unknown endpoint %q", name)
		}
	}
	return set, nil
}

func (c Config) nativeEnabled(name string) bool {
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

// NativeAdvertised reports whether the named native endpoint is both enabled and selected for advertisement.
func (c Config) NativeAdvertised(name string) bool {
	return c.nativeEnabled(name) && (c.AdvertiseAllNative || c.AdvertisedNative[name])
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

func validOrigin(value, scheme string, allowSelf bool) bool {
	if allowSelf && value == "self" {
		return true
	}
	u, err := url.Parse(value)
	return err == nil && (scheme == "" || u.Scheme == scheme) && (u.Scheme == "http" || u.Scheme == "https") && u.Hostname() != "" && u.Path == "" && u.User == nil && u.RawQuery == "" && !u.ForceQuery && u.Fragment == ""
}

// Validate returns the first inconsistency in the configuration, or nil.
func (c Config) Validate() error {
	if err := c.validateAuth(); err != nil {
		return err
	}
	if err := c.validateLimits(); err != nil {
		return err
	}
	if err := c.validateListeners(); err != nil {
		return err
	}
	return c.validatePublicOrigins()
}

func (c Config) validateLimits() error {
	for _, limit := range []struct {
		name  string
		value int
	}{{"GM_MAX_ACTIVE_MEASUREMENTS", c.MaxActiveMeasurements}, {"GM_MAX_ACTIVE_MEASUREMENTS_PER_CLIENT", c.MaxActiveMeasurementsPerClient}, {"GM_MAX_ACTIVE_SESSIONS", c.MaxActiveSessions}, {"GM_MAX_SESSIONS_PER_CLIENT", c.MaxSessionsPerClient}, {"GM_MAX_CONNECTIONS", c.MaxConnections}, {"GM_MAX_CONNECTIONS_PER_CLIENT", c.MaxConnectionsPerClient}} {
		if limit.value <= 0 {
			return fmt.Errorf("%s must be greater than zero", limit.name)
		}
	}
	if c.MaxActiveMeasurementsPerClient > c.MaxActiveMeasurements {
		return fmt.Errorf("GM_MAX_ACTIVE_MEASUREMENTS_PER_CLIENT must not exceed GM_MAX_ACTIVE_MEASUREMENTS")
	}
	// The session budget is a share of the global pool, not an extension of it.
	if c.MaxActiveSessions > c.MaxActiveMeasurements {
		return fmt.Errorf("GM_MAX_ACTIVE_SESSIONS must not exceed GM_MAX_ACTIVE_MEASUREMENTS")
	}
	if c.MaxSessionsPerClient > c.MaxActiveMeasurementsPerClient {
		return fmt.Errorf("GM_MAX_SESSIONS_PER_CLIENT must not exceed GM_MAX_ACTIVE_MEASUREMENTS_PER_CLIENT")
	}
	// One client must not be able to take the whole session budget.
	if c.MaxSessionsPerClient > c.MaxActiveSessions {
		return fmt.Errorf("GM_MAX_SESSIONS_PER_CLIENT must not exceed GM_MAX_ACTIVE_SESSIONS")
	}
	if c.MaxConnectionsPerClient > c.MaxConnections {
		return fmt.Errorf("GM_MAX_CONNECTIONS_PER_CLIENT must not exceed GM_MAX_CONNECTIONS")
	}
	if c.MaxOperationDuration <= 0 {
		return fmt.Errorf("GM_MAX_OPERATION_DURATION must be greater than zero")
	}
	if c.MaxSessionDuration < c.MaxOperationDuration {
		return fmt.Errorf("GM_MAX_SESSION_DURATION must be at least GM_MAX_OPERATION_DURATION")
	}
	return nil
}

func (c Config) validateListeners() error {
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
		for name := range maps.Keys(c.AdvertisedNative) {
			if !c.nativeEnabled(name) {
				return fmt.Errorf("GM_ADVERTISED_NATIVE_ENDPOINTS includes disabled endpoint %q", name)
			}
		}
	}
	return nil
}

func (c Config) validatePublicOrigins() error {
	for _, native := range []struct{ name, value, scheme string }{{"GM_H1_PUBLIC_ORIGIN", c.NativePublic.H1, "http"}, {"GM_H1_TLS_PUBLIC_ORIGIN", c.NativePublic.H1TLS, "https"}, {"GM_H2_PUBLIC_ORIGIN", c.NativePublic.H2, "https"}, {"GM_H3_PUBLIC_ORIGIN", c.NativePublic.H3, "https"}} {
		if native.value != "" && !validOrigin(native.value, native.scheme, false) {
			return fmt.Errorf("%s must be an origin with %s scheme", native.name, native.scheme)
		}
	}
	for _, list := range []struct {
		name   string
		values []string
	}{{"GM_PUBLIC_ORIGINS", c.Public.Both}, {"GM_PUBLIC_THROUGHPUT_ORIGINS", c.Public.Throughput}, {"GM_PUBLIC_LATENCY_ORIGINS", c.Public.Latency}} {
		for _, value := range list.values {
			if !validOrigin(value, "", true) {
				return fmt.Errorf("%s contains invalid origin %q", list.name, value)
			}
		}
	}
	if err := c.validateNoNativePublicOriginClash(); err != nil {
		return err
	}
	if !c.NativeAdvertised(NativeH1Clear) && !c.NativeAdvertised(NativeH1TLS) && !c.NativeAdvertised(NativeH2) && !c.NativeAdvertised(NativeH3) && len(c.Public.Both) == 0 && len(c.Public.Throughput) == 0 {
		return fmt.Errorf("configuration advertises no throughput endpoint")
	}
	return nil
}

func (c Config) validateNoNativePublicOriginClash() error {
	deterministic := map[string]string{}
	for _, endpoint := range []struct{ name, origin, protocol string }{{NativeH1Clear, c.NativePublic.H1, "http1"}, {NativeH1TLS, c.NativePublic.H1TLS, "http1"}, {NativeH2, c.NativePublic.H2, "http2"}, {NativeH3, c.NativePublic.H3, "http3"}} {
		if endpoint.origin == "" || !c.NativeAdvertised(endpoint.name) {
			continue
		}
		key := origin.Key(endpoint.origin)
		if protocol, ok := deterministic[key]; ok && protocol != endpoint.protocol {
			return fmt.Errorf("native origin %q is advertised with multiple deterministic protocols", endpoint.origin)
		}
		deterministic[key] = endpoint.protocol
	}
	for _, origins := range [][]string{c.Public.Both, c.Public.Throughput} {
		for _, publicOrigin := range origins {
			if _, ok := deterministic[origin.Key(publicOrigin)]; ok {
				return fmt.Errorf("origin %q cannot be both native deterministic and public negotiated", publicOrigin)
			}
		}
	}
	return nil
}

func (c Config) validateAuth() error {
	a := c.Auth
	switch a.Mode {
	case "off", "password", "oidc", "hybrid":
	default:
		return fmt.Errorf("GM_AUTH_MODE must be off, password, oidc, or hybrid")
	}
	if a.Mode == "off" {
		if a.configured() {
			return fmt.Errorf("authentication settings require GM_AUTH_MODE to be enabled")
		}
		return nil
	}
	publicURL, err := a.validatePublicURL()
	if err != nil {
		return err
	}
	if err := a.validatePassword(); err != nil {
		return err
	}
	if err := a.validateOIDC(); err != nil {
		return err
	}
	return c.validateAdvertisedAuthOrigins(publicURL)
}

func (a AuthConfig) configured() bool {
	return a.Explicit || a.PublicURL != "" || a.PasswordHash != "" || a.PasswordHashFile != "" ||
		a.OIDCIssuer != "" || a.OIDCClientID != "" || a.OIDCClientSecret != "" || a.OIDCSecretFile != "" ||
		len(a.OIDCAllowedGroups) != 0 || a.OIDCProviderName != "Authelia"
}

func (a AuthConfig) validatePublicURL() (*url.URL, error) {
	if !validOrigin(a.PublicURL, "https", false) {
		return nil, fmt.Errorf("GM_AUTH_PUBLIC_URL must be an HTTPS origin with no path, query, or fragment")
	}
	// validOrigin already parsed this value, so the error cannot recur.
	publicURL, _ := url.Parse(a.PublicURL)
	if publicURL.Port() == "443" {
		return nil, fmt.Errorf("GM_AUTH_PUBLIC_URL must omit the default HTTPS port")
	}
	return publicURL, nil
}

func (a AuthConfig) validatePassword() error {
	if a.PasswordHash != "" && a.PasswordHashFile != "" {
		return fmt.Errorf("GM_AUTH_PASSWORD_HASH and GM_AUTH_PASSWORD_HASH_FILE are mutually exclusive")
	}
	wantsPassword := a.Mode == "password" || a.Mode == "hybrid"
	if wantsPassword == (a.PasswordHash != "" || a.PasswordHashFile != "") {
		return nil
	}
	if wantsPassword {
		return fmt.Errorf("password authentication requires exactly one password hash source")
	}
	return fmt.Errorf("password hash configured while password authentication is disabled")
}

func (a AuthConfig) validateOIDC() error {
	if a.OIDCClientSecret != "" && a.OIDCSecretFile != "" {
		return fmt.Errorf("GM_AUTH_OIDC_CLIENT_SECRET and GM_AUTH_OIDC_CLIENT_SECRET_FILE are mutually exclusive")
	}
	wantsOIDC := a.Mode == "oidc" || a.Mode == "hybrid"
	if wantsOIDC && !a.oidcComplete() {
		return fmt.Errorf("OIDC authentication requires issuer, client ID, one client secret source, and allowed groups")
	}
	if !wantsOIDC && a.oidcConfigured() {
		return fmt.Errorf("OIDC settings configured while OIDC authentication is disabled")
	}
	if wantsOIDC && !validOIDCIssuer(a.OIDCIssuer) {
		return fmt.Errorf("GM_AUTH_OIDC_ISSUER must be an HTTPS URL with no credentials, query, or fragment")
	}
	return a.validateProviderName(wantsOIDC)
}

// oidcConfigured reports whether any OIDC setting is present at all.
func (a AuthConfig) oidcConfigured() bool {
	return a.OIDCIssuer != "" || a.OIDCClientID != "" || a.OIDCClientSecret != "" ||
		a.OIDCSecretFile != "" || len(a.OIDCAllowedGroups) != 0
}

// oidcComplete reports whether every setting an OIDC login needs is present.
func (a AuthConfig) oidcComplete() bool {
	return a.OIDCIssuer != "" && a.OIDCClientID != "" &&
		(a.OIDCClientSecret != "" || a.OIDCSecretFile != "") && len(a.OIDCAllowedGroups) != 0
}

// validOIDCIssuer accepts as the issuer only a bare HTTPS origin or path, with no credentials, query, or fragment.
func validOIDCIssuer(raw string) bool {
	u, err := url.Parse(raw)
	return err == nil && u.Scheme == "https" && u.Hostname() != "" &&
		u.User == nil && u.RawQuery == "" && u.Fragment == ""
}

func (a AuthConfig) validateProviderName(wantsOIDC bool) error {
	if wantsOIDC && strings.TrimSpace(a.OIDCProviderName) == "" {
		return fmt.Errorf("GM_AUTH_OIDC_PROVIDER_NAME must not be empty")
	}
	if len(a.OIDCProviderName) > 64 {
		return fmt.Errorf("GM_AUTH_OIDC_PROVIDER_NAME must be at most 64 bytes without control characters")
	}
	for _, r := range a.OIDCProviderName {
		if unicode.IsControl(r) {
			return fmt.Errorf("GM_AUTH_OIDC_PROVIDER_NAME must be at most 64 bytes without control characters")
		}
	}
	return nil
}

func (c Config) validateAdvertisedAuthOrigins(publicURL *url.URL) error {
	if c.NativeAdvertised(NativeH1Clear) {
		return fmt.Errorf("clear HTTP/1.1 cannot be advertised when authentication is enabled")
	}
	check := func(name, value string) error {
		if value == "" || value == "self" {
			return nil
		}
		u, err := url.Parse(value)
		if err != nil || u.Scheme != "https" || !strings.EqualFold(u.Hostname(), publicURL.Hostname()) {
			return fmt.Errorf("%s must use HTTPS and the canonical authentication hostname", name)
		}
		return nil
	}
	for _, native := range []struct{ name, value string }{
		{"GM_H1_TLS_PUBLIC_ORIGIN", c.NativePublic.H1TLS}, {"GM_H2_PUBLIC_ORIGIN", c.NativePublic.H2}, {"GM_H3_PUBLIC_ORIGIN", c.NativePublic.H3},
	} {
		if err := check(native.name, native.value); err != nil {
			return err
		}
	}
	for _, list := range []struct {
		name   string
		values []string
	}{
		{"GM_PUBLIC_ORIGINS", c.Public.Both}, {"GM_PUBLIC_THROUGHPUT_ORIGINS", c.Public.Throughput}, {"GM_PUBLIC_LATENCY_ORIGINS", c.Public.Latency},
	} {
		for _, value := range list.values {
			if err := check(list.name, value); err != nil {
				return err
			}
		}
	}
	return nil
}
