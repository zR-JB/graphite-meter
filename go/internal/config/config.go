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

// NativeEndpoints holds one value per native endpoint: a listen address (empty disables it) or an advertised origin.
type NativeEndpoints struct {
	H1, H1TLS, H2, H3 string
}

// PublicOrigins holds proxied origins advertised for both measurement kinds, for throughput only, or for latency only.
type PublicOrigins struct {
	Both, Throughput, Latency []string
}

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
		MaxActiveSessions: 64, MaxSessionsPerClient: 8,
		MaxConnections: 512, MaxConnectionsPerClient: 64,
		MaxOperationDuration: 5 * time.Minute,
		MaxSessionDuration:   2 * time.Hour,
		Auth:                 AuthConfig{Mode: "off", OIDCProviderName: "Authelia"},
	}
}

// Native is one native endpoint's listen address, advertised origin and fixed protocol.
type Native struct {
	Name, Env, Addr, Public, Scheme, Protocol string
}

// Natives lists the native endpoints in a fixed order; Env prefixes their GM_*_ADDR and GM_*_PUBLIC_ORIGIN.
func (c Config) Natives() []Native {
	return []Native{
		{NativeH1Clear, "GM_H1", c.Native.H1, c.NativePublic.H1, "http", "http1"},
		{NativeH1TLS, "GM_H1_TLS", c.Native.H1TLS, c.NativePublic.H1TLS, "https", "http1"},
		{NativeH2, "GM_H2", c.Native.H2, c.NativePublic.H2, "https", "http2"},
		{NativeH3, "GM_H3", c.Native.H3, c.NativePublic.H3, "https", "http3"},
	}
}

func (c Config) TLSEnabled() bool {
	return c.Native.H1TLS != "" || c.Native.H2 != "" || c.Native.H3 != ""
}

// NativeAdvertised reports whether the named native endpoint is both enabled and selected for advertisement.
func (c Config) NativeAdvertised(name string) bool {
	return c.nativeEnabled(name) && (c.AdvertisedNative == nil || c.AdvertisedNative[name])
}

func (c Config) nativeEnabled(name string) bool {
	return slices.ContainsFunc(c.Natives(), func(n Native) bool { return n.Name == name && n.Addr != "" })
}

type publicList struct {
	env     string
	origins []string
}

func (c Config) publicLists() []publicList {
	return []publicList{
		{"GM_PUBLIC_ORIGINS", c.Public.Both},
		{"GM_PUBLIC_THROUGHPUT_ORIGINS", c.Public.Throughput},
		{"GM_PUBLIC_LATENCY_ORIGINS", c.Public.Latency},
	}
}

// setting is one operator setting: its environment variable and, except for secrets, its flag.
type setting struct {
	env, flag, usage string
	boolean          bool
	set              func(string) error
	show             func() string
}

// field binds a setting to p; an empty value keeps the default unless keepEmpty.
func field[T any](env, flag, usage string, p *T, parse func(string) (T, error), keepEmpty bool) setting {
	return setting{env: env, flag: flag, usage: usage,
		set: func(raw string) error {
			value := strings.TrimSpace(raw)
			if value == "" && !keepEmpty {
				return nil
			}
			parsed, err := parse(value)
			if err == nil {
				*p = parsed
			}
			return err
		},
		show: func() string {
			var zero T
			if s := fmt.Sprint(*p); s != fmt.Sprint(zero) {
				return s
			}
			return ""
		},
	}
}

func text(env, flag, usage string, p *string) setting {
	return field(env, flag, usage, p, func(v string) (string, error) { return v, nil }, true)
}

func list(env, flag, usage string, p *[]string) setting {
	return field(env, flag, usage, p, func(v string) ([]string, error) { return splitList(v), nil }, true)
}

func number(env, flag, usage string, p *int) setting {
	return field(env, flag, usage, p, func(v string) (int, error) {
		n, err := strconv.Atoi(v)
		if err != nil {
			return 0, errors.New("must be an integer")
		}
		return n, nil
	}, false)
}

func duration(env, flag, usage string, p *time.Duration) setting {
	return field(env, flag, usage, p, time.ParseDuration, false)
}

func boolean(env, flag, usage string, p *bool) setting {
	s := field(env, flag, usage, p, func(v string) (bool, error) {
		b, err := strconv.ParseBool(strings.ToLower(v))
		if err != nil {
			return false, errors.New("must be true/false or 1/0")
		}
		return b, nil
	}, false)
	s.boolean = true
	return s
}

func (c *Config) settings() []setting {
	a := &c.Auth
	return []setting{
		text("GM_H1_ADDR", "h1-addr", "clear HTTP/1.1 listen `address`", &c.Native.H1),
		text("GM_H1_TLS_ADDR", "h1-tls-addr", "HTTPS HTTP/1.1 listen `address`; empty disables it", &c.Native.H1TLS),
		text("GM_H2_ADDR", "h2-addr", "HTTP/2 TLS listen `address`; empty disables it", &c.Native.H2),
		text("GM_H3_ADDR", "h3-addr", "HTTP/3 UDP and bootstrap TCP listen `address`; empty disables it", &c.Native.H3),
		text("GM_TLS_CERT", "tls-cert", "TLS certificate PEM `path`", &c.TLSCert),
		text("GM_TLS_KEY", "tls-key", "TLS private key PEM `path`", &c.TLSKey),
		text("GM_H1_PUBLIC_ORIGIN", "h1-public-origin",
			"public `origin` of the native clear HTTP/1.1 listener", &c.NativePublic.H1),
		text("GM_H1_TLS_PUBLIC_ORIGIN", "h1-tls-public-origin",
			"public `origin` of the native HTTPS HTTP/1.1 listener", &c.NativePublic.H1TLS),
		text("GM_H2_PUBLIC_ORIGIN", "h2-public-origin",
			"public `origin` of the native HTTP/2 listener", &c.NativePublic.H2),
		text("GM_H3_PUBLIC_ORIGIN", "h3-public-origin",
			"public `origin` of the native HTTP/3 listener", &c.NativePublic.H3),
		field("GM_ADVERTISED_NATIVE_ENDPOINTS", "advertised-native-endpoints",
			"all, none, or comma-separated native endpoint `names`", &c.AdvertisedNative, parseAdvertisedNative, true),
		list("GM_PUBLIC_ORIGINS", "public-origins",
			"comma-separated negotiated `origins` providing throughput and latency", &c.Public.Both),
		list("GM_PUBLIC_THROUGHPUT_ORIGINS", "public-throughput-origins",
			"comma-separated negotiated throughput `origins`", &c.Public.Throughput),
		list("GM_PUBLIC_LATENCY_ORIGINS", "public-latency-origins",
			"comma-separated WebSocket latency `origins`", &c.Public.Latency),
		text("GM_SERVER_NAME", "name", "server `name` advertised in /preflight", &c.ServerName),
		text("GM_SERVER_LOCATION", "location", "server `location` label", &c.ServerLocation),
		boolean("GM_RESULT_HISTORY_DEFAULT", "result-history-default",
			"save completed browser results on this device by default", &c.ResultHistoryDefault),
		boolean("GM_VERBOSE", "verbose", "log per-second download/upload throughput", &c.Verbose),
		number("GM_MAX_ACTIVE_MEASUREMENTS", "max-active-measurements",
			"maximum `number` of concurrent measurement handlers", &c.MaxActiveMeasurements),
		number("GM_MAX_ACTIVE_MEASUREMENTS_PER_CLIENT", "max-active-measurements-per-client",
			"maximum `number` of concurrent measurement handlers per client", &c.MaxActiveMeasurementsPerClient),
		number("GM_MAX_ACTIVE_SESSIONS", "max-active-sessions",
			"maximum `number` of concurrent WebTransport sessions, a share of the measurement pool",
			&c.MaxActiveSessions),
		number("GM_MAX_SESSIONS_PER_CLIENT", "max-sessions-per-client",
			"maximum `number` of concurrent WebTransport sessions per client", &c.MaxSessionsPerClient),
		number("GM_MAX_CONNECTIONS", "max-connections",
			"maximum `number` of concurrent TCP and QUIC connections", &c.MaxConnections),
		number("GM_MAX_CONNECTIONS_PER_CLIENT", "max-connections-per-client",
			"maximum `number` of concurrent connections per direct client", &c.MaxConnectionsPerClient),
		duration("GM_MAX_OPERATION_DURATION", "max-operation-duration",
			"maximum measurement operation `duration`", &c.MaxOperationDuration),
		duration("GM_MAX_SESSION_DURATION", "max-session-duration",
			"maximum WebTransport session `duration`", &c.MaxSessionDuration),
		{env: "GM_TRUSTED_PROXIES", set: c.parseTrustedProxies},
		text("GM_AUTH_MODE", "auth-mode", "authentication `mode`: off, password, oidc, or hybrid", &a.Mode),
		text("GM_AUTH_PUBLIC_URL", "auth-public-url", "canonical HTTPS UI `origin`", &a.PublicURL),
		text("GM_AUTH_PASSWORD_HASH", "", "", &a.PasswordHash),
		text("GM_AUTH_PASSWORD_HASH_FILE", "auth-password-hash-file",
			"`file` containing the operator Argon2id PHC hash", &a.PasswordHashFile),
		text("GM_AUTH_OIDC_ISSUER", "auth-oidc-issuer", "OIDC issuer `URL`", &a.OIDCIssuer),
		text("GM_AUTH_OIDC_CLIENT_ID", "auth-oidc-client-id", "OIDC client `ID`", &a.OIDCClientID),
		text("GM_AUTH_OIDC_CLIENT_SECRET", "", "", &a.OIDCClientSecret),
		text("GM_AUTH_OIDC_CLIENT_SECRET_FILE", "auth-oidc-client-secret-file",
			"`file` containing the OIDC client secret", &a.OIDCSecretFile),
		list("GM_AUTH_OIDC_ALLOWED_GROUPS", "auth-oidc-allowed-groups",
			"comma-separated case-sensitive OIDC `groups`", &a.OIDCAllowedGroups),
		text("GM_AUTH_OIDC_PROVIDER_NAME", "auth-oidc-provider-name", "OIDC provider `label`", &a.OIDCProviderName),
	}
}

// apply sets a value from either source; any GM_AUTH_* setting but the mode marks auth explicit.
func (c *Config) apply(s setting, value string) error {
	if strings.HasPrefix(s.env, "GM_AUTH_") && s.env != "GM_AUTH_MODE" {
		c.Auth.Explicit = true
	}
	return s.set(value)
}

func Load() (Config, error) {
	c := Default()
	for _, s := range c.settings() {
		if v, ok := os.LookupEnv(s.env); ok {
			if err := c.apply(s, v); err != nil {
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

func RegisterFlags(fs *flag.FlagSet, c *Config) {
	for _, s := range c.settings() {
		if s.flag != "" {
			fs.Var(flagValue{c, s}, s.flag, s.usage)
		}
	}
}

type flagValue struct {
	c *Config
	s setting
}

func (v flagValue) String() string {
	if v.c == nil {
		return ""
	}
	return v.s.show()
}

func (v flagValue) Set(value string) error { return v.c.apply(v.s, value) }

func (v flagValue) IsBoolFlag() bool { return v.s.boolean }

func (c *Config) parseTrustedProxies(v string) error {
	c.TrustedProxies = nil
	for _, raw := range splitList(v) {
		prefix, err := netip.ParsePrefix(raw)
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

// parseAdvertisedNative maps "all" to nil (every endpoint) and "none" or "" to an empty set.
func parseAdvertisedNative(raw string) (map[string]bool, error) {
	switch strings.TrimSpace(raw) {
	case "all":
		return nil, nil
	case "", "none":
		return map[string]bool{}, nil
	}
	set := map[string]bool{}
	for _, name := range splitList(raw) {
		if !slices.Contains([]string{NativeH1Clear, NativeH1TLS, NativeH2, NativeH3}, name) {
			return nil, fmt.Errorf("unknown endpoint %q", name)
		}
		set[name] = true
	}
	return set, nil
}

// validOrigin accepts what clients accept as an origin, optionally of one scheme.
func validOrigin(value, scheme string) bool {
	canonical, err := wire.CanonicalOrigin(value)
	return err == nil && (scheme == "" || strings.HasPrefix(canonical, scheme+"://"))
}

func (c Config) Validate() error {
	if len(c.ServerCatalog.Servers) > 0 {
		if err := c.ServerCatalog.Validate(); err != nil {
			return err
		}
	}
	checks := []func() error{c.validateAuth, c.validateLimits, c.validateListeners, c.validatePublicOrigins}
	for _, check := range checks {
		if err := check(); err != nil {
			return err
		}
	}
	return nil
}

func (c Config) validateLimits() error {
	for _, limit := range []struct {
		env   string
		value int
	}{
		{"GM_MAX_ACTIVE_MEASUREMENTS", c.MaxActiveMeasurements},
		{"GM_MAX_ACTIVE_MEASUREMENTS_PER_CLIENT", c.MaxActiveMeasurementsPerClient},
		{"GM_MAX_ACTIVE_SESSIONS", c.MaxActiveSessions},
		{"GM_MAX_SESSIONS_PER_CLIENT", c.MaxSessionsPerClient},
		{"GM_MAX_CONNECTIONS", c.MaxConnections},
		{"GM_MAX_CONNECTIONS_PER_CLIENT", c.MaxConnectionsPerClient},
	} {
		if limit.value <= 0 {
			return fmt.Errorf("%s must be greater than zero", limit.env)
		}
	}
	// Sessions are a share of the pool, and no client may take the whole session budget.
	for _, pair := range []struct {
		inner, outer string
		a, b         int
	}{
		{"GM_MAX_ACTIVE_MEASUREMENTS_PER_CLIENT", "GM_MAX_ACTIVE_MEASUREMENTS",
			c.MaxActiveMeasurementsPerClient, c.MaxActiveMeasurements},
		{"GM_MAX_ACTIVE_SESSIONS", "GM_MAX_ACTIVE_MEASUREMENTS", c.MaxActiveSessions, c.MaxActiveMeasurements},
		{"GM_MAX_SESSIONS_PER_CLIENT", "GM_MAX_ACTIVE_MEASUREMENTS_PER_CLIENT",
			c.MaxSessionsPerClient, c.MaxActiveMeasurementsPerClient},
		{"GM_MAX_SESSIONS_PER_CLIENT", "GM_MAX_ACTIVE_SESSIONS", c.MaxSessionsPerClient, c.MaxActiveSessions},
		{"GM_MAX_CONNECTIONS_PER_CLIENT", "GM_MAX_CONNECTIONS", c.MaxConnectionsPerClient, c.MaxConnections},
	} {
		if pair.a > pair.b {
			return fmt.Errorf("%s must not exceed %s", pair.inner, pair.outer)
		}
	}
	if c.MaxOperationDuration <= 0 {
		return errors.New("GM_MAX_OPERATION_DURATION must be greater than zero")
	}
	if c.MaxSessionDuration < c.MaxOperationDuration {
		return errors.New("GM_MAX_SESSION_DURATION must be at least GM_MAX_OPERATION_DURATION")
	}
	return nil
}

func (c Config) validateListeners() error {
	if c.Native.H1 == "" {
		return errors.New("GM_H1_ADDR must not be empty")
	}
	if c.TLSEnabled() && (c.TLSCert == "" || c.TLSKey == "") {
		return errors.New("GM_TLS_CERT and GM_TLS_KEY are required when a native TLS listener is enabled")
	}
	natives := c.Natives()
	for i, a := range natives {
		for _, b := range natives[i+1:] {
			if a.Addr != "" && a.Addr == b.Addr {
				return fmt.Errorf("%s_ADDR and %s_ADDR must differ", a.Env, b.Env)
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
	throughput := len(c.Public.Both) > 0 || len(c.Public.Throughput) > 0
	deterministic := map[string]string{}
	for _, n := range c.Natives() {
		if n.Public != "" && !validOrigin(n.Public, n.Scheme) {
			return fmt.Errorf("%s_PUBLIC_ORIGIN must be an origin with %s scheme", n.Env, n.Scheme)
		}
		if !c.NativeAdvertised(n.Name) {
			continue
		}
		throughput = true
		if n.Public == "" {
			continue
		}
		key := origin.Key(n.Public)
		if protocol, ok := deterministic[key]; ok && protocol != n.Protocol {
			return fmt.Errorf("native origin %q is advertised with multiple deterministic protocols", n.Public)
		}
		deterministic[key] = n.Protocol
	}
	for _, l := range c.publicLists() {
		for _, value := range l.origins {
			if value != "self" && !validOrigin(value, "") {
				return fmt.Errorf("%s contains invalid origin %q", l.env, value)
			}
			if _, ok := deterministic[origin.Key(value)]; ok && l.env != "GM_PUBLIC_LATENCY_ORIGINS" {
				return fmt.Errorf("origin %q cannot be both native deterministic and public negotiated", value)
			}
		}
	}
	if !throughput {
		return errors.New("configuration advertises no throughput endpoint")
	}
	return nil
}

func (c Config) validateAuth() error {
	a := c.Auth
	switch a.Mode {
	case "off":
		if a.Explicit || a.PublicURL != "" || a.passwordConfigured() || a.oidcConfigured() ||
			a.OIDCProviderName != "Authelia" {
			return errors.New("authentication settings require GM_AUTH_MODE to be enabled")
		}
		return nil
	case "password", "oidc", "hybrid":
	default:
		return errors.New("GM_AUTH_MODE must be off, password, oidc, or hybrid")
	}
	if !validOrigin(a.PublicURL, "https") {
		return errors.New("GM_AUTH_PUBLIC_URL must be an HTTPS origin with no path, query, or fragment")
	}
	publicURL, _ := url.Parse(a.PublicURL)
	if publicURL.Port() == "443" {
		return errors.New("GM_AUTH_PUBLIC_URL must omit the default HTTPS port")
	}
	if err := a.validateSecrets(); err != nil {
		return err
	}
	return c.validateAdvertisedAuthOrigins(publicURL.Hostname())
}

func (a AuthConfig) passwordConfigured() bool {
	return a.PasswordHash != "" || a.PasswordHashFile != ""
}

func (a AuthConfig) oidcConfigured() bool {
	return a.OIDCIssuer != "" || a.OIDCClientID != "" || a.OIDCClientSecret != "" ||
		a.OIDCSecretFile != "" || len(a.OIDCAllowedGroups) != 0
}

func (a AuthConfig) validateSecrets() error {
	if a.PasswordHash != "" && a.PasswordHashFile != "" {
		return errors.New("GM_AUTH_PASSWORD_HASH and GM_AUTH_PASSWORD_HASH_FILE are mutually exclusive")
	}
	if a.OIDCClientSecret != "" && a.OIDCSecretFile != "" {
		return errors.New("GM_AUTH_OIDC_CLIENT_SECRET and GM_AUTH_OIDC_CLIENT_SECRET_FILE are mutually exclusive")
	}
	wantsPassword, wantsOIDC := a.Mode != "oidc", a.Mode != "password"
	switch {
	case wantsPassword && !a.passwordConfigured():
		return errors.New("password authentication requires exactly one password hash source")
	case !wantsPassword && a.passwordConfigured():
		return errors.New("password hash configured while password authentication is disabled")
	case wantsOIDC && (a.OIDCIssuer == "" || a.OIDCClientID == "" || len(a.OIDCAllowedGroups) == 0 ||
		a.OIDCClientSecret == "" && a.OIDCSecretFile == ""):
		return errors.New(
			"OIDC authentication requires issuer, client ID, one client secret source, and allowed groups")
	case !wantsOIDC && a.oidcConfigured():
		return errors.New("OIDC settings configured while OIDC authentication is disabled")
	}
	if wantsOIDC && !validOIDCIssuer(a.OIDCIssuer) {
		return errors.New("GM_AUTH_OIDC_ISSUER must be an HTTPS URL with no credentials, query, or fragment")
	}
	if wantsOIDC && strings.TrimSpace(a.OIDCProviderName) == "" {
		return errors.New("GM_AUTH_OIDC_PROVIDER_NAME must not be empty")
	}
	if len(a.OIDCProviderName) > 64 || strings.ContainsFunc(a.OIDCProviderName, unicode.IsControl) {
		return errors.New("GM_AUTH_OIDC_PROVIDER_NAME must be at most 64 bytes without control characters")
	}
	return nil
}

func validOIDCIssuer(raw string) bool {
	u, err := url.Parse(raw)
	return err == nil && u.Scheme == "https" && u.Hostname() != "" && u.User == nil && u.RawQuery == "" &&
		u.Fragment == ""
}

func (c Config) validateAdvertisedAuthOrigins(hostname string) error {
	if c.NativeAdvertised(NativeH1Clear) {
		return errors.New("clear HTTP/1.1 cannot be advertised when authentication is enabled")
	}
	lists := c.publicLists()
	for _, n := range c.Natives()[1:] {
		lists = append(lists, publicList{n.Env + "_PUBLIC_ORIGIN", []string{n.Public}})
	}
	for _, l := range lists {
		for _, value := range l.origins {
			if value == "" || value == "self" {
				continue
			}
			u, err := url.Parse(value)
			if err != nil || u.Scheme != "https" || !strings.EqualFold(u.Hostname(), hostname) {
				return fmt.Errorf("%s must use HTTPS and the canonical authentication hostname", l.env)
			}
		}
	}
	return nil
}
