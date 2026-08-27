// Package auth implements the authentication boundary.
package auth

import (
	"context"
	"errors"
	"fmt"
	"io"
	"log"
	"net/http"
	"net/netip"
	"net/url"
	"os"
	"strings"
	"sync"
	"sync/atomic"
	"time"

	"github.com/zR-JB/graphite-meter/go/internal/config"
)

type authCounters struct{ local, oidc, invalidPassword, oidcFailure, groupDenial, replayExpiry, throttled, logout, cliApproval, capacity atomic.Uint64 }

type Service struct {
	cfg            config.AuthConfig
	public         *url.URL
	trusted        []netip.Prefix
	passwordHash   string
	argon          chan struct{}
	mu             sync.Mutex
	sessions       map[[32]byte]*session
	grants         map[[32]byte]*session
	wtTokens       map[[32]byte]wtToken
	attempts       map[string]loginAttempt
	exchanges      map[string]loginAttempt
	globalAttempts []time.Time
	ceilingLogged  map[string]time.Time
	approvals      map[string]*cliApproval
	oidc           *oidcState
	now            func() time.Time
	verbose        bool
	counters       authCounters
	connectSrc     string
}

func authModes(mode string) (password, oidc bool) {
	return mode == "password" || mode == "hybrid", mode == "oidc" || mode == "hybrid"
}

func (s *Service) SetConnectOrigins(origins []string) {
	s.connectSrc = strings.Join(origins, " ")
}

func New(ctx context.Context, cfg config.AuthConfig, trusted []netip.Prefix, verbose bool) (*Service, error) {
	s := &Service{cfg: cfg, trusted: trusted, sessions: map[[32]byte]*session{}, grants: map[[32]byte]*session{}, wtTokens: map[[32]byte]wtToken{}, attempts: map[string]loginAttempt{}, exchanges: map[string]loginAttempt{}, ceilingLogged: map[string]time.Time{}, approvals: map[string]*cliApproval{}, argon: make(chan struct{}, 2), now: time.Now, verbose: verbose}
	if cfg.Mode == "off" {
		return s, nil
	}
	var err error
	s.public, err = url.Parse(cfg.PublicURL)
	if err != nil {
		return nil, err
	}
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
		s.oidc = newOIDCState(cfg, secret, s.verbose)
		if !password {
			discovery, err := s.oidc.discover(ctx, s.public)
			if err != nil {
				return nil, fmt.Errorf("OIDC discovery: %w", err)
			}
			s.oidc.install(discovery)
			log.Printf("[gm:auth] OIDC provider ready")
		} else {
			s.oidc.startRetry(ctx, s.public)
		}
	}
	log.Printf("[gm:auth] mode=%s origin=%s provider=%s issuer=%s allowed-groups=%d session-lifetime=%s", cfg.Mode, cfg.PublicURL, cfg.OIDCProviderName, cfg.OIDCIssuer, len(cfg.OIDCAllowedGroups), sessionLifetime)
	go s.sweep(ctx)
	go s.runSecurityLog(ctx)
	return s, nil
}

func (s *Service) debugln(message string) {
	if s.verbose {
		log.Printf("[gm:auth:debug] %s", message)
	}
}

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

// Enabled reports whether the authentication boundary is in force.
func (s *Service) Enabled() bool { return s.cfg.Mode != "off" }

// PublicOrigin is the canonical origin the boundary accepts, or "" when authentication is off.
func (s *Service) PublicOrigin() string {
	if s.public == nil {
		return ""
	}
	return s.public.String()
}

func (s *Service) Mount(mux *http.ServeMux) {
	if !s.Enabled() {
		mux.HandleFunc("/login", http.NotFound)
		mux.HandleFunc("/auth/", http.NotFound)
		return
	}
	mux.HandleFunc("GET /login", s.loginPage)
	password, oidc := authModes(s.cfg.Mode)
	if password {
		mux.HandleFunc("POST /auth/password", s.passwordLogin)
	}
	if oidc {
		mux.HandleFunc("POST /auth/oidc/start", s.oidcStart)
		mux.HandleFunc("GET /auth/oidc/callback", s.oidcCallback)
	}
	mux.HandleFunc("GET /auth/session", s.sessionInfo)
	mux.HandleFunc("POST /auth/logout", s.logout)
	mux.HandleFunc("GET /auth/cli", s.cliPage)
	mux.HandleFunc("POST /auth/cli/approve", s.cliApprove)
	mux.HandleFunc("POST /auth/cli/token", s.cliToken)
	mux.HandleFunc("/login", http.NotFound)
	mux.HandleFunc("/auth/", http.NotFound)
}

func (s *Service) runSecurityLog(ctx context.Context) {
	t := time.Tick(time.Minute)
	var last [10]uint64
	for {
		select {
		case <-ctx.Done():
			return
		case <-t:
			values := [10]uint64{s.counters.local.Load(), s.counters.oidc.Load(), s.counters.invalidPassword.Load(), s.counters.oidcFailure.Load(), s.counters.groupDenial.Load(), s.counters.replayExpiry.Load(), s.counters.throttled.Load(), s.counters.logout.Load(), s.counters.cliApproval.Load(), s.counters.capacity.Load()}
			if values == last {
				continue
			}
			var delta [10]uint64
			for i := range values {
				delta[i] = values[i] - last[i]
			}
			last = values
			log.Printf("[gm:auth] 1m local=%d oidc=%d invalid-password=%d oidc-failure=%d group-denial=%d replay-expiry=%d throttled=%d logout=%d cli-approval=%d capacity=%d", delta[0], delta[1], delta[2], delta[3], delta[4], delta[5], delta[6], delta[7], delta[8], delta[9])
		}
	}
}
