// Package auth implements the authentication boundary.
package auth

import (
	"context"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/netip"
	"net/url"
	"os"
	"strings"
	"sync"
	"sync/atomic"
	"time"

	"github.com/zR-JB/graphite-meter/go/internal/config"
	"github.com/zR-JB/graphite-meter/go/internal/logx"
	"github.com/zR-JB/graphite-meter/go/internal/static"
)

type counter int

const (
	countLocal counter = iota
	countOIDC
	countInvalidPassword
	countOIDCFailure
	countGroupDenial
	countReplayExpiry
	countThrottled
	countLogout
	countCLIApproval
	countCapacity
	counters
)

var counterNames = [counters]string{"local", "oidc", "invalid-password", "oidc-failure", "group-denial",
	"replay-expiry", "throttled", "logout", "cli-approval", "capacity"}

type Service struct {
	cfg              config.AuthConfig
	public           *url.URL
	origin           string // public.String(), or "" when authentication is off
	trusted          []netip.Prefix
	passwordHash     string
	argon            chan struct{}
	mu               sync.Mutex
	sessions         map[[32]byte]*session
	grants           map[[32]byte]*grant
	grantSeq         uint64
	socketTokens     map[[32]byte]socketToken
	attempts         map[string][]time.Time
	exchanges        map[string][]time.Time
	approvalAttempts map[string][]time.Time
	oidcStarts       map[string][]time.Time
	globalAttempts   []time.Time
	ceilingLogged    map[string]time.Time
	approvals        map[string]*approval
	oidc             *oidcState
	verbose          bool
	counters         [counters]atomic.Uint64
	pagePolicy       string
}

// logSummary names how people sign in, one line per concern, with only the settings the mode uses.
func logSummary(cfg config.AuthConfig, sessionLifetime time.Duration) {
	password, oidc := authModes(cfg.Mode)
	how := map[[2]bool]string{{true, false}: "password", {false, true}: "OIDC", {true, true}: "password and OIDC"}
	logx.Infof("auth", "%s sign-in at %s; sessions last %s", how[[2]bool{password, oidc}], cfg.PublicURL,
		strings.TrimSuffix(strings.TrimSuffix(sessionLifetime.String(), "0s"), "0m"))
	if oidc {
		groups := "every group allowed"
		if n := len(cfg.OIDCAllowedGroups); n > 0 {
			groups = fmt.Sprintf("%d %s allowed", n, map[bool]string{true: "group", false: "groups"}[n == 1])
		}
		logx.Infof("auth", "OIDC provider %s at %s; %s", cfg.OIDCProviderName, cfg.OIDCIssuer, groups)
	}
}

func authModes(mode string) (password, oidc bool) {
	return mode == "password" || mode == "hybrid", mode == "oidc" || mode == "hybrid"
}

func (s *Service) SetConnectOrigins(origins []string) {
	s.pagePolicy = static.PagePolicy(origins)
}

// PagePolicy is the client shell's policy under authentication, or "" in public mode.
func (s *Service) PagePolicy() string { return s.pagePolicy }

func New(ctx context.Context, cfg config.AuthConfig, trusted []netip.Prefix, verbose bool) (*Service, error) {
	s := &Service{
		cfg:              cfg,
		trusted:          trusted,
		sessions:         map[[32]byte]*session{},
		grants:           map[[32]byte]*grant{},
		socketTokens:     map[[32]byte]socketToken{},
		attempts:         map[string][]time.Time{},
		exchanges:        map[string][]time.Time{},
		approvalAttempts: map[string][]time.Time{},
		oidcStarts:       map[string][]time.Time{},
		ceilingLogged:    map[string]time.Time{},
		approvals:        map[string]*approval{},
		argon:            make(chan struct{}, 2),
		verbose:          verbose,
	}
	if cfg.Mode == "off" {
		return s, nil
	}
	var err error
	s.public, err = url.Parse(cfg.PublicURL)
	if err != nil {
		return nil, err
	}
	s.origin = s.public.String()
	password, oidc := authModes(cfg.Mode)
	if password {
		s.passwordHash, err = readSecret(cfg.PasswordHash, cfg.PasswordHashFile, 4096)
		if err != nil {
			return nil, fmt.Errorf("password hash: %w", err)
		}
		if _, _, err := parsePasswordHash(s.passwordHash); err != nil {
			return nil, err
		}
		s.debugln("local password hash loaded and validated")
	}
	if oidc {
		secret, e := readSecret(cfg.OIDCClientSecret, cfg.OIDCSecretFile, 16*1024)
		if e != nil {
			return nil, fmt.Errorf("OIDC client secret: %w", e)
		}
		s.oidc = newOIDCState(ctx, cfg, secret, s.verbose)
		if !password {
			discovery, err := s.oidc.discover(ctx, s.public)
			if err != nil {
				return nil, fmt.Errorf("OIDC discovery: %w", err)
			}
			s.oidc.discovered.Store(discovery)
			logx.Infof("auth", "OIDC provider ready")
		} else {
			go s.oidc.retryDiscovery(ctx, s.public)
		}
	}
	logSummary(cfg, sessionLifetime)
	go s.sweep(ctx)
	go s.runSecurityLog(ctx)
	return s, nil
}

func (s *Service) debugln(message string) { debugln(s.verbose, message) }

func debugln(verbose bool, message string) {
	if verbose {
		logx.Debugf("auth", "%s", message)
	}
}

func (s *Service) count(c counter) { s.counters[c].Add(1) }

func readSecret(inline, file string, limit int64) (string, error) {
	if inline != "" {
		return strings.TrimSpace(inline), nil
	}
	f, err := os.Open(file)
	if err != nil {
		return "", err
	}
	defer f.Close()
	b, err := io.ReadAll(io.LimitReader(f, limit+1))
	if err != nil {
		return "", err
	}
	if int64(len(b)) > limit {
		return "", fmt.Errorf("secret file exceeds %d bytes", limit)
	}
	v := strings.TrimSpace(string(b))
	if v == "" {
		return "", errors.New("secret is empty")
	}
	return v, nil
}

func (s *Service) Enabled() bool { return s.cfg.Mode != "off" }

// PublicOrigin is the canonical origin the boundary accepts, or "" when authentication is off.
func (s *Service) PublicOrigin() string { return s.origin }

// PublicHostname is the canonical origin's hostname, or "" when authentication is off.
func (s *Service) PublicHostname() string {
	if s.public == nil {
		return ""
	}
	return s.public.Hostname()
}

func (s *Service) Mount(mux *http.ServeMux) {
	if !s.Enabled() {
		mux.HandleFunc("/login", http.NotFound)
		mux.HandleFunc("/auth/", http.NotFound)
		return
	}
	page := func(pattern string, h http.HandlerFunc) {
		mux.HandleFunc(pattern, func(w http.ResponseWriter, r *http.Request) {
			securityHeaders(w.Header())
			r.Body = http.MaxBytesReader(w, r.Body, 4096)
			h(w, r)
		})
	}
	page("GET /login", s.loginPage)
	password, oidc := authModes(s.cfg.Mode)
	if password {
		page("POST /auth/password", s.passwordLogin)
	}
	if oidc {
		page("POST /auth/oidc/start", s.oidcStart)
		page("GET /auth/oidc/callback", s.oidcCallback)
	}
	page("GET /auth/session", s.sessionInfo)
	page("POST /auth/logout", s.logout)
	page("GET /auth/browser", s.browserPage)
	page("POST /auth/browser/approve", s.approve)
	page("POST /auth/browser/token", s.token)
	page("GET /auth/cli", s.cliPage)
	page("POST /auth/cli/approve", s.approve)
	page("POST /auth/cli/token", s.token)
	page("/login", http.NotFound)
	page("/auth/", http.NotFound)
}

func (s *Service) runSecurityLog(ctx context.Context) {
	t := time.Tick(time.Minute)
	var last [counters]uint64
	for {
		select {
		case <-ctx.Done():
			return
		case <-t:
			var line strings.Builder
			changed := false
			for i := range s.counters {
				value := s.counters[i].Load()
				changed = changed || value != last[i]
				fmt.Fprintf(&line, " %s=%d", counterNames[i], value-last[i])
				last[i] = value
			}
			if changed {
				logx.Infof("auth", "1m%s", line.String())
			}
		}
	}
}
