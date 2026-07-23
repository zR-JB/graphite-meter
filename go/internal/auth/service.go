// Package auth implements Graphite Meter's optional authentication boundary.
//
// The package is split by security responsibility, one auditable claim per
// file: wrap.go (nothing gets through without a principal), trust.go (what we
// believe about a request and why), session.go (sessions bounded, hashed,
// revocable), headers.go (response-header policy), ratelimit.go (attempt
// budgets), reasons.go (why an attempt failed), handlers.go (the login
// surface), oidc.go, password.go, cli.go. This file is wiring only.
//
// # Why authentication lives in the process
//
// The obvious alternative is forward-auth: put Authelia or oauth2-proxy in
// front of the server behind nginx or Traefik and keep zero authentication
// code here. For most web applications that is the better answer. It is the
// wrong answer for this one, for three reasons specific to what the product
// is.
//
//   - A proxy in the data path perturbs the measurement. This is an
//     instrument: its output is a claim about a network path, and every extra
//     hop that terminates, buffers, and re-originates the transfer becomes
//     part of what is being measured. An authentication decision must not
//     appear in the numbers.
//   - Forward-auth does not compose with the HTTP/3 listener. QUIC is UDP;
//     the subrequest-authentication mechanisms of the common proxies are
//     HTTP-over-TCP constructs, and there is no equivalent in front of a QUIC
//     listener that leaves the transport characteristics intact.
//   - Measurement runs across several origins and ports at once — cleartext
//     H1, H1-TLS, H2, H3 bootstrap, H3/QUIC, and a WebSocket — and the
//     credential has to be coherent across all of them, including for the
//     native client, which holds a bearer grant rather than a cookie. A
//     cookie-issuing proxy in front of one origin cannot express that.
//
// So the boundary is in-process, applied outermost on every listener
// (listeners.go), and the five explicit Enforce call sites are deliberately
// not collapsed into a loop: they are the enforcement audit.
//
// What is still delegated is everything a library does better: OIDC discovery
// and token verification (coreos/go-oidc), the OAuth2 exchange (x/oauth2),
// and password hashing (x/crypto/argon2). What is written here is what no
// library provides safely for this shape: the session store, the CSRF and
// origin policy, the enforcement boundary itself, and the native-client
// grant flow.
package auth

import (
	"context"
	"errors"
	"fmt"
	"html/template"
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

// Service owns every piece of authentication state. All of it lives in memory
// only, and every store is bounded and swept:
//
//	sessions       ≤ maxSessions (1024) total, ≤ maxSubjectSessions (8) per
//	               subject, each ≤ sessionLifetime (8h); swept every minute.
//	grants         ≤ 8 per session; deleted with their session.
//	approvals      ≤ 256 total, ≤ 8 per session, each ≤ 2 minutes; swept on
//	               every /auth/cli page load and with their session.
//	attempts       ≤ maxBudgetKeys (2048) keys, ≤ maxAddressAttempts (5) per
//	               key per minute; keyed per IPv4 address and per IPv6 /64.
//	globalAttempts ≤ maxGlobalAttempts (60) password attempts per minute
//	               across all addresses.
//	exchanges      ≤ maxBudgetKeys keys, ≤ maxAddressExchanges (10) OIDC token
//	               exchanges per key per minute, keyed the same way.
//	ceilingLogged  one timestamp per global ceiling; fixed size.
//	oidc.tx        ≤ 256 transactions total, ≤ 8 per client address, each
//	               ≤ 10 minutes; swept on every /auth/oidc/start.
//
// Every bound trades availability for a store an anonymous caller cannot grow.
type Service struct {
	ctx            context.Context
	cfg            config.AuthConfig
	public         *url.URL
	trusted        []netip.Prefix
	passwordHash   string
	argon          chan struct{}
	mu             sync.Mutex
	sessions       map[[32]byte]*session
	grants         map[[32]byte]*session
	attempts       map[string]loginAttempt
	exchanges      map[string]loginAttempt
	globalAttempts []time.Time
	ceilingLogged  map[string]time.Time
	approvals      map[string]*cliApproval
	oidc           *oidcState
	loginTemplate  *template.Template
	now            func() time.Time
	verbose        bool
	counters       authCounters
}

func New(ctx context.Context, cfg config.AuthConfig, trusted []netip.Prefix, verbose bool) (*Service, error) {
	s := &Service{ctx: ctx, cfg: cfg, trusted: trusted, sessions: map[[32]byte]*session{}, grants: map[[32]byte]*session{}, attempts: map[string]loginAttempt{}, exchanges: map[string]loginAttempt{}, ceilingLogged: map[string]time.Time{}, approvals: map[string]*cliApproval{}, argon: make(chan struct{}, 2), now: time.Now, verbose: verbose}
	if cfg.Mode == "off" {
		return s, nil
	}
	var err error
	s.public, err = url.Parse(cfg.PublicURL)
	if err != nil {
		return nil, err
	}
	if cfg.Mode == "password" || cfg.Mode == "hybrid" {
		s.passwordHash, err = readSecret(cfg.PasswordHash, cfg.PasswordHashFile, 4096)
		if err != nil {
			return nil, fmt.Errorf("password hash: %w", err)
		}
		if _, _, err := parsePasswordHash(s.passwordHash); err != nil {
			return nil, err
		}
		s.debugln("local password hash loaded and validated")
	}
	s.loginTemplate = loginTemplate
	if cfg.Mode == "oidc" || cfg.Mode == "hybrid" {
		secret, e := readSecret(cfg.OIDCClientSecret, cfg.OIDCSecretFile, 16*1024)
		if e != nil {
			return nil, fmt.Errorf("OIDC client secret: %w", e)
		}
		s.oidc = newOIDCState(cfg, secret, s.verbose)
		if cfg.Mode == "oidc" {
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

func (s *Service) Enabled() bool { return s.cfg.Mode != "off" }

func (s *Service) PublicOrigin() string {
	if s.public == nil {
		return ""
	}
	return s.public.String()
}

// Mount installs the auth routes. Every route it registers still passes
// through Enforce; Mount only decides which paths exist at all.
func (s *Service) Mount(mux *http.ServeMux) {
	if !s.Enabled() {
		mux.HandleFunc("/login", http.NotFound)
		mux.HandleFunc("/auth/", http.NotFound)
		return
	}
	mux.HandleFunc("GET /login", s.loginPage)
	if s.cfg.Mode == "password" || s.cfg.Mode == "hybrid" {
		mux.HandleFunc("POST /auth/password", s.passwordLogin)
	}
	if s.cfg.Mode == "oidc" || s.cfg.Mode == "hybrid" {
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

// runSecurityLog emits a one-line delta of the auth counters each minute, and
// stays silent when nothing changed.
func (s *Service) runSecurityLog(ctx context.Context) {
	t := time.NewTicker(time.Minute)
	defer t.Stop()
	var last [10]uint64
	for {
		select {
		case <-ctx.Done():
			return
		case <-t.C:
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
