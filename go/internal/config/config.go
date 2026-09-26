// Package config loads and validates server configuration.
package config

import (
	"errors"
	"flag"
	"fmt"
	"maps"
	"net/netip"
	"net/url"
	"os"
	"slices"
	"strconv"
	"strings"
	"time"
	"unicode"

	"github.com/zR-JB/graphite-meter/go/internal/origin"
	"github.com/zR-JB/graphite-meter/go/internal/wire"
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

var nativeNames = []string{NativeH1Clear, NativeH1TLS, NativeH2, NativeH3}

// NativeEndpoints holds one value per native endpoint: a listen address (empty disables it) or an advertised origin.
type NativeEndpoints struct {
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
	ServerCatalog wire.ServerCatalog
	Native        NativeEndpoints // listen addresses
	NativePublic  NativeEndpoints // advertised origins
	// AdvertisedNative selects the native endpoints preflight advertises; nil selects all.
	AdvertisedNative                          map[string]bool
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
		ServerCatalog: wire.SingletonCatalog(),
		Native:        NativeEndpoints{H1: ":7246"},
		ServerName:    "graphite-meter", EngineVersion: EngineVersion,
		MaxActiveMeasurements: 256, MaxActiveMeasurementsPerClient: 32,
		MaxActiveSessions: 64, MaxSessionsPerClient: 16,
		MaxConnections: 512, MaxConnectionsPerClient: 64,
		MaxOperationDuration: 5 * time.Minute,
		MaxSessionDuration:   2 * time.Hour,
		Auth:                 AuthConfig{Mode: "off", OIDCProviderName: "Authelia"},
	}
}

// setting is one operator setting: its environment variable and, except for secrets, its flag.
type setting struct {
	env, flag, usage string
	boolean          bool
	apply            func(*Config, string) error
	show             func(*Config) string
}

// field binds a setting to a Config field; an empty value keeps the default unless emptyMeaningful.
func field[T any](env, flag, usage string, at func(*Config) *T, parse func(string) (T, error), emptyMeaningful bool) setting {
	return setting{env: env, flag: flag, usage: usage,
		apply: func(c *Config, raw string) error {
			value := strings.TrimSpace(raw)
			if value == "" && !emptyMeaningful {
				return nil
			}
			parsed, err := parse(value)
			if err == nil {
				*at(c) = parsed
			}
			return err
		},
		show: func(c *Config) string {
			var zero T
			if s := fmt.Sprint(*at(c)); s != fmt.Sprint(zero) {
				return s
			}
			return ""
		},
	}
}

func text(env, flag, usage string, at func(*Config) *string) setting {
	return field(env, flag, usage, at, func(v string) (string, error) { return v, nil }, true)
}

func list(env, flag, usage string, at func(*Config) *[]string) setting {
	return field(env, flag, usage, at, func(v string) ([]string, error) { return splitList(v), nil }, true)
}

func number(env, flag, usage string, at func(*Config) *int) setting {
	return field(env, flag, usage, at, func(v string) (int, error) {
		n, err := strconv.Atoi(v)
		if err != nil {
			return 0, errors.New("must be an integer")
		}
		return n, nil
	}, false)
}

func duration(env, flag, usage string, at func(*Config) *time.Duration) setting {
	return field(env, flag, usage, at, time.ParseDuration, false)
}

func boolean(env, flag, usage string, at func(*Config) *bool) setting {
	s := field(env, flag, usage, at, func(v string) (bool, error) {
		b, err := strconv.ParseBool(strings.ToLower(v))
		if err != nil {
			return false, errors.New("must be true/false or 1/0")
		}
		return b, nil
	}, false)
	s.boolean = true
	return s
}

var settings = []setting{
	text("GM_H1_ADDR", "h1-addr", "clear HTTP/1.1 listen `address`", func(c *Config) *string { return &c.Native.H1 }),
	text("GM_H1_TLS_ADDR", "h1-tls-addr", "HTTPS HTTP/1.1 listen `address`; empty disables it", func(c *Config) *string { return &c.Native.H1TLS }),
	text("GM_H2_ADDR", "h2-addr", "HTTP/2 TLS listen `address`; empty disables it", func(c *Config) *string { return &c.Native.H2 }),
	text("GM_H3_ADDR", "h3-addr", "HTTP/3 UDP and bootstrap TCP listen `address`; empty disables it", func(c *Config) *string { return &c.Native.H3 }),
	text("GM_TLS_CERT", "tls-cert", "TLS certificate PEM `path`", func(c *Config) *string { return &c.TLSCert }),
	text("GM_TLS_KEY", "tls-key", "TLS private key PEM `path`", func(c *Config) *string { return &c.TLSKey }),
	text("GM_H1_PUBLIC_ORIGIN", "h1-public-origin", "public `origin` of the native clear HTTP/1.1 listener", func(c *Config) *string { return &c.NativePublic.H1 }),
	text("GM_H1_TLS_PUBLIC_ORIGIN", "h1-tls-public-origin", "public `origin` of the native HTTPS HTTP/1.1 listener", func(c *Config) *string { return &c.NativePublic.H1TLS }),
	text("GM_H2_PUBLIC_ORIGIN", "h2-public-origin", "public `origin` of the native HTTP/2 listener", func(c *Config) *string { return &c.NativePublic.H2 }),
	text("GM_H3_PUBLIC_ORIGIN", "h3-public-origin", "public `origin` of the native HTTP/3 listener", func(c *Config) *string { return &c.NativePublic.H3 }),
	{env: "GM_ADVERTISED_NATIVE_ENDPOINTS", flag: "advertised-native-endpoints", usage: "all, none, or comma-separated native endpoint `names`",
		apply: func(c *Config, v string) (err error) {
			c.AdvertisedNative, err = ParseAdvertisedNative(v)
			return err
		}, show: func(*Config) string { return "" }},
	list("GM_PUBLIC_ORIGINS", "public-origins", "comma-separated negotiated `origins` providing throughput and latency", func(c *Config) *[]string { return &c.Public.Both }),
	list("GM_PUBLIC_THROUGHPUT_ORIGINS", "public-throughput-origins", "comma-separated negotiated throughput `origins`", func(c *Config) *[]string { return &c.Public.Throughput }),
	list("GM_PUBLIC_LATENCY_ORIGINS", "public-latency-origins", "comma-separated WebSocket latency `origins`", func(c *Config) *[]string { return &c.Public.Latency }),
	text("GM_SERVER_NAME", "name", "server `name` advertised in /preflight", func(c *Config) *string { return &c.ServerName }),
	text("GM_SERVER_LOCATION", "location", "server `location` label", func(c *Config) *string { return &c.ServerLocation }),
	boolean("GM_RESULT_HISTORY_DEFAULT", "result-history-default", "save completed browser results on this device by default", func(c *Config) *bool { return &c.ResultHistoryDefault }),
	boolean("GM_VERBOSE", "verbose", "log per-second download/upload throughput", func(c *Config) *bool { return &c.Verbose }),
	number("GM_MAX_ACTIVE_MEASUREMENTS", "max-active-measurements", "maximum `number` of concurrent measurement handlers", func(c *Config) *int { return &c.MaxActiveMeasurements }),
	number("GM_MAX_ACTIVE_MEASUREMENTS_PER_CLIENT", "max-active-measurements-per-client", "maximum `number` of concurrent measurement handlers per client", func(c *Config) *int { return &c.MaxActiveMeasurementsPerClient }),
	number("GM_MAX_ACTIVE_SESSIONS", "max-active-sessions", "maximum `number` of concurrent WebTransport sessions, a share of the measurement pool", func(c *Config) *int { return &c.MaxActiveSessions }),
	number("GM_MAX_SESSIONS_PER_CLIENT", "max-sessions-per-client", "maximum `number` of concurrent WebTransport sessions per client", func(c *Config) *int { return &c.MaxSessionsPerClient }),
	number("GM_MAX_CONNECTIONS", "max-connections", "maximum `number` of concurrent TCP and QUIC connections", func(c *Config) *int { return &c.MaxConnections }),
	number("GM_MAX_CONNECTIONS_PER_CLIENT", "max-connections-per-client", "maximum `number` of concurrent connections per direct client", func(c *Config) *int { return &c.MaxConnectionsPerClient }),
	duration("GM_MAX_OPERATION_DURATION", "max-operation-duration", "maximum measurement operation `duration`", func(c *Config) *time.Duration { return &c.MaxOperationDuration }),
	duration("GM_MAX_SESSION_DURATION", "max-session-duration", "maximum WebTransport session `duration`", func(c *Config) *time.Duration { return &c.MaxSessionDuration }),
	{env: "GM_TRUSTED_PROXIES", apply: parseTrustedProxies},
	text("GM_AUTH_MODE", "auth-mode", "authentication `mode`: off, password, oidc, or hybrid", func(c *Config) *string { return &c.Auth.Mode }),
	text("GM_AUTH_PUBLIC_URL", "auth-public-url", "canonical HTTPS UI `origin`", func(c *Config) *string { return &c.Auth.PublicURL }),
	text("GM_AUTH_PASSWORD_HASH", "", "", func(c *Config) *string { return &c.Auth.PasswordHash }),
	text("GM_AUTH_PASSWORD_HASH_FILE", "auth-password-hash-file", "`file` containing the operator Argon2id PHC hash", func(c *Config) *string { return &c.Auth.PasswordHashFile }),
	text("GM_AUTH_OIDC_ISSUER", "auth-oidc-issuer", "OIDC issuer `URL`", func(c *Config) *string { return &c.Auth.OIDCIssuer }),
	text("GM_AUTH_OIDC_CLIENT_ID", "auth-oidc-client-id", "OIDC client `ID`", func(c *Config) *string { return &c.Auth.OIDCClientID }),
	text("GM_AUTH_OIDC_CLIENT_SECRET", "", "", func(c *Config) *string { return &c.Auth.OIDCClientSecret }),
	text("GM_AUTH_OIDC_CLIENT_SECRET_FILE", "auth-oidc-client-secret-file", "`file` containing the OIDC client secret", func(c *Config) *string { return &c.Auth.OIDCSecretFile }),
	list("GM_AUTH_OIDC_ALLOWED_GROUPS", "auth-oidc-allowed-groups", "comma-separated case-sensitive OIDC `groups`", func(c *Config) *[]string { return &c.Auth.OIDCAllowedGroups }),
	text("GM_AUTH_OIDC_PROVIDER_NAME", "auth-oidc-provider-name", "OIDC provider `label`", func(c *Config) *string { return &c.Auth.OIDCProviderName }),
}

// set applies a value from either source; any GM_AUTH_* setting but the mode marks auth explicit.
func (s *setting) set(c *Config, value string) error {
	if strings.HasPrefix(s.env, "GM_AUTH_") && s.env != "GM_AUTH_MODE" {
		c.Auth.Explicit = true
	}
	return s.apply(c, value)
}

// Load reads every setting from its environment variable over the defaults.
func Load() (Config, error) {
	c := Default()
	for i := range settings {
		s := &settings[i]
		if v, ok := os.LookupEnv(s.env); ok {
			if err := s.set(&c, v); err != nil {
				return Config{}, fmt.Errorf("%s: %w", s.env, err)
			}
		}
	}
	var err error
	if c.ServerCatalog, err = loadServerCatalog(); err != nil {
		return Config{}, err
	}
	return c, nil
}

// RegisterFlags binds every setting that has a flag to fs, over c's current values.
func RegisterFlags(fs *flag.FlagSet, c *Config) {
	for i := range settings {
		if s := &settings[i]; s.flag != "" {
			fs.Var(flagValue{c, s}, s.flag, s.usage)
		}
	}
}

type flagValue struct {
	c *Config
	s *setting
}

func (v flagValue) String() string {
	if v.s == nil {
		return ""
	}
	return v.s.show(v.c)
}

func (v flagValue) Set(value string) error { return v.s.set(v.c, value) }

func (v flagValue) IsBoolFlag() bool { return v.s != nil && v.s.boolean }

func parseTrustedProxies(c *Config, v string) error {
	c.TrustedProxies = nil
	if strings.TrimSpace(v) == "" {
		return nil
	}
	for raw := range strings.SplitSeq(v, ",") {
		prefix, err := netip.ParsePrefix(strings.TrimSpace(raw))
		if err != nil {
			return fmt.Errorf("%q: %w", raw, err)
		}
		if prefix.Bits() == 0 {
			return fmt.Errorf("%q trusts every address; list the proxy's actual CIDR instead", raw)
		}
		c.TrustedProxies = append(c.TrustedProxies, prefix.Masked())
	}
	return nil
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

// ParseAdvertisedNative maps "all" to nil (every endpoint) and "none" or "" to an empty set.
func ParseAdvertisedNative(raw string) (map[string]bool, error) {
	switch strings.TrimSpace(raw) {
	case "all":
		return nil, nil
	case "", "none":
		return map[string]bool{}, nil
	}
	set := map[string]bool{}
	for _, name := range splitList(raw) {
		if !slices.Contains(nativeNames, name) {
			return nil, fmt.Errorf("unknown endpoint %q", name)
		}
		set[name] = true
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
	return c.nativeEnabled(name) && (c.AdvertisedNative == nil || c.AdvertisedNative[name])
}

// validOrigin accepts what clients accept as an origin, optionally of one scheme.
func validOrigin(value, scheme string) bool {
	canonical, err := wire.CanonicalOrigin(value)
	return err == nil && (scheme == "" || strings.HasPrefix(canonical, scheme+"://"))
}

// Validate returns the first inconsistency in the configuration, or nil.
func (c Config) Validate() error {
	if len(c.ServerCatalog.Servers) > 0 {
		if err := c.ServerCatalog.Validate(); err != nil {
			return err
		}
	}
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
	for name := range maps.Keys(c.AdvertisedNative) {
		if !c.nativeEnabled(name) {
			return fmt.Errorf("GM_ADVERTISED_NATIVE_ENDPOINTS includes disabled endpoint %q", name)
		}
	}
	return nil
}

func (c Config) validatePublicOrigins() error {
	for _, native := range []struct{ name, value, scheme string }{{"GM_H1_PUBLIC_ORIGIN", c.NativePublic.H1, "http"}, {"GM_H1_TLS_PUBLIC_ORIGIN", c.NativePublic.H1TLS, "https"}, {"GM_H2_PUBLIC_ORIGIN", c.NativePublic.H2, "https"}, {"GM_H3_PUBLIC_ORIGIN", c.NativePublic.H3, "https"}} {
		if native.value != "" && !validOrigin(native.value, native.scheme) {
			return fmt.Errorf("%s must be an origin with %s scheme", native.name, native.scheme)
		}
	}
	for _, list := range []struct {
		name   string
		values []string
	}{{"GM_PUBLIC_ORIGINS", c.Public.Both}, {"GM_PUBLIC_THROUGHPUT_ORIGINS", c.Public.Throughput}, {"GM_PUBLIC_LATENCY_ORIGINS", c.Public.Latency}} {
		for _, value := range list.values {
			if value != "self" && !validOrigin(value, "") {
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
	if !validOrigin(a.PublicURL, "https") {
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
