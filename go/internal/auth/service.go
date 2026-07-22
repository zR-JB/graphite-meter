// Package auth implements Graphite Meter's optional authentication boundary.
package auth

import (
	"context"
	"crypto/rand"
	"crypto/sha256"
	"crypto/subtle"
	"encoding/base64"
	"encoding/json"
	"errors"
	"fmt"
	"html/template"
	"io"
	"log"
	"net"
	"net/http"
	"net/netip"
	"net/url"
	"os"
	"sort"
	"strings"
	"sync"
	"sync/atomic"
	"time"

	"github.com/zR-JB/graphite-meter/go/internal/config"
)

const (
	sessionCookie      = "__Host-gm_session"
	csrfCookie         = "__Host-gm_csrf"
	loginCookie        = "__Host-gm_login"
	transactionCookie  = "__Host-gm_oidc"
	maxSessions        = 1024
	maxSubjectSessions = 8
	sessionLifetime    = 8 * time.Hour
)

type Listener struct{ UI, Clear bool }

type principalKey struct{}

var errSessionEnded = errors.New("authentication session ended")

type Principal struct {
	Subject, Name, Provider string
	Expires                 time.Time
	session                 *session
	Bearer                  bool
}

type session struct {
	hash                    [32]byte
	subject, name, provider string
	expires, created        time.Time
	ctx                     context.Context
	cancel                  context.CancelFunc
	grants                  map[[32]byte]struct{}
	csrf                    string
}

type loginAttempt struct {
	times []time.Time
}
type cliApproval struct {
	challenge string
	code      string
	session   *session
	expires   time.Time
	approved  bool
}
type authCounters struct{ local, oidc, invalidPassword, oidcFailure, groupDenial, replayExpiry, throttled, logout, cliApproval, capacity atomic.Uint64 }

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
	globalAttempts []time.Time
	approvals      map[string]*cliApproval
	oidc           *oidcState
	loginTemplate  *template.Template
	now            func() time.Time
	verbose        bool
	counters       authCounters
}

func New(ctx context.Context, cfg config.AuthConfig, trusted []netip.Prefix, verbose ...bool) (*Service, error) {
	s := &Service{ctx: ctx, cfg: cfg, trusted: trusted, sessions: map[[32]byte]*session{}, grants: map[[32]byte]*session{}, attempts: map[string]loginAttempt{}, approvals: map[string]*cliApproval{}, argon: make(chan struct{}, 2), now: time.Now, verbose: len(verbose) != 0 && verbose[0]}
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
		s.debugf("local password hash loaded and validated")
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

func (s *Service) debugf(message string) {
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

func (s *Service) Mount(mux *http.ServeMux) {
	if !s.Enabled() {
		mux.HandleFunc("/login", http.NotFound)
		mux.HandleFunc("/auth/", http.NotFound)
		return
	}
	mux.HandleFunc("GET /login", s.login)
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

func (s *Service) Wrap(next http.Handler, listener Listener) http.Handler {
	if !s.Enabled() {
		return next
	}
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		secure, canonical := s.secureCanonical(r)
		if secure && r.TLS != nil && !strings.EqualFold(requestHostname(r.Host), s.public.Hostname()) {
			s.authRequired(w, r, listener)
			return
		}
		if secure {
			authenticatedSecurityHeaders(w.Header())
		}
		if r.Method == http.MethodPost && strings.HasPrefix(r.URL.Path, "/auth/") {
			controller := http.NewResponseController(w)
			_ = controller.SetReadDeadline(s.now().Add(15 * time.Second))
			defer controller.SetReadDeadline(time.Time{})
		}
		if r.Method == http.MethodOptions && isMeasurementRoute(r.URL.Path) {
			s.preflight(w, r, secure)
			return
		}
		if (r.URL.Path == "/login" || strings.HasPrefix(r.URL.Path, "/auth/")) && (!listener.UI || !canonical) {
			forbidden(w)
			return
		}
		public := listener.UI && s.isPublicAuthRoute(r.Method, r.URL.Path)
		if public {
			if !secure || !canonical {
				s.authRequired(w, r, listener)
				return
			}
			next.ServeHTTP(w, r)
			return
		}
		if !secure {
			s.authRequired(w, r, listener)
			return
		}
		p, ok := s.authenticate(r)
		if !ok {
			s.authRequired(w, r, listener)
			return
		}
		if p.Bearer && !isMeasurementRoute(r.URL.Path) {
			forbidden(w)
			return
		}
		if p.session != nil {
			ctx, cancel := context.WithCancelCause(r.Context())
			stop := context.AfterFunc(p.session.ctx, func() { cancel(errSessionEnded) })
			defer func() { stop(); cancel(nil) }()
			r = r.WithContext(context.WithValue(ctx, principalKey{}, p))
		} else {
			r = r.WithContext(context.WithValue(r.Context(), principalKey{}, p))
		}
		if !s.validRequestOrigin(r, p) {
			forbidden(w)
			return
		}
		next.ServeHTTP(w, r)
	})
}

func requestHostname(host string) string {
	u, err := url.Parse("//" + host)
	if err != nil {
		return ""
	}
	return u.Hostname()
}

func (s *Service) isPublicAuthRoute(method, path string) bool {
	if method == http.MethodGet && (path == "/login" || path == "/auth/cli") || method == http.MethodPost && path == "/auth/cli/token" {
		return true
	}
	if (s.cfg.Mode == "password" || s.cfg.Mode == "hybrid") && method == http.MethodPost && path == "/auth/password" {
		return true
	}
	return (s.cfg.Mode == "oidc" || s.cfg.Mode == "hybrid") && (method == http.MethodPost && path == "/auth/oidc/start" || method == http.MethodGet && path == "/auth/oidc/callback")
}
func isMeasurementRoute(path string) bool {
	switch path {
	case "/preflight", "/probe", "/download", "/upload/session", "/upload", "/upload/progress", "/ws/ping":
		return true
	}
	return false
}

func (s *Service) secureCanonical(r *http.Request) (bool, bool) {
	if r.TLS != nil {
		return true, equalHost(r.Host, s.public.Host)
	}
	peer, err := splitRemote(r.RemoteAddr)
	if err != nil || !prefixContains(s.trusted, peer) {
		return false, false
	}
	proto := singleHeader(r.Header, "X-Forwarded-Proto")
	host := singleHeader(r.Header, "X-Forwarded-Host")
	return proto == "https" && equalHost(host, s.public.Host), proto == "https" && equalHost(host, s.public.Host)
}

func singleHeader(h http.Header, name string) string {
	v := h.Values(name)
	if len(v) != 1 || strings.Contains(v[0], ",") {
		return ""
	}
	return strings.TrimSpace(v[0])
}
func equalHost(a, b string) bool {
	return strings.EqualFold(strings.TrimSuffix(a, "."), strings.TrimSuffix(b, "."))
}
func splitRemote(raw string) (netip.Addr, error) {
	host, _, err := net.SplitHostPort(raw)
	if err != nil {
		host = raw
	}
	return netip.ParseAddr(strings.Trim(host, "[]"))
}
func prefixContains(ps []netip.Prefix, a netip.Addr) bool {
	if a.Is4In6() {
		a = a.Unmap()
	}
	for _, p := range ps {
		if p.Contains(a) {
			return true
		}
	}
	return false
}

func (s *Service) authRequired(w http.ResponseWriter, r *http.Request, listener Listener) {
	securityHeaders(w.Header())
	if s.public != nil && r.Header.Get("Origin") == s.public.String() {
		h := w.Header()
		h.Set("Access-Control-Allow-Origin", s.public.String())
		h.Set("Access-Control-Allow-Credentials", "true")
		h.Set("Access-Control-Expose-Headers", "Graphite-Meter-Auth, Graphite-Meter-Auth-URL")
		h.Set("Timing-Allow-Origin", s.public.String())
		h.Add("Vary", "Origin")
	}
	w.Header().Set("Graphite-Meter-Auth", "required")
	w.Header().Set("Graphite-Meter-Auth-URL", s.public.String()+"/login")
	if r.ProtoMajor == 1 && r.Body != nil {
		w.Header().Set("Connection", "close")
	}
	if listener.UI && r.Method == http.MethodGet && r.URL.Path == "/" {
		s.debugf("unauthenticated UI root redirected to login")
		http.Redirect(w, r, s.public.String()+"/login", http.StatusTemporaryRedirect)
		return
	}
	w.WriteHeader(http.StatusForbidden)
}

func forbidden(w http.ResponseWriter) {
	securityHeaders(w.Header())
	w.WriteHeader(http.StatusForbidden)
}
func securityHeaders(h http.Header) {
	h.Set("Cache-Control", "no-store")
	h.Set("Referrer-Policy", "same-origin")
	h.Set("X-Content-Type-Options", "nosniff")
	h.Set("Permissions-Policy", "camera=(), microphone=(), geolocation=()")
	h.Set("Content-Security-Policy", authPageCSP(""))
}

func authPageCSP(authorizationOrigin string) string {
	formAction := "'self'"
	if authorizationOrigin != "" {
		formAction += " " + authorizationOrigin
	}
	return "default-src 'none'; style-src 'sha256-" + authStyleHash + "'; form-action " + formAction + "; frame-ancestors 'none'; base-uri 'none'"
}

func (s *Service) loginSecurityHeaders(h http.Header) {
	securityHeaders(h)
	if s.oidc != nil {
		h.Set("Content-Security-Policy", authPageCSP(s.oidc.authorizationOrigin()))
	}
}

func authenticatedSecurityHeaders(h http.Header) {
	h.Set("Strict-Transport-Security", "max-age=31536000")
	h.Set("X-Frame-Options", "DENY")
	h.Set("Content-Security-Policy", "frame-ancestors 'none'")
	h.Set("Referrer-Policy", "same-origin")
	h.Set("X-Content-Type-Options", "nosniff")
	h.Set("Permissions-Policy", "camera=(), microphone=(), geolocation=()")
}

func (s *Service) preflight(w http.ResponseWriter, r *http.Request, secure bool) {
	if !secure || r.Header.Get("Origin") != s.public.String() {
		forbidden(w)
		return
	}
	method := r.Header.Get("Access-Control-Request-Method")
	if !allowedPreflightMethod(r.URL.Path, method) {
		forbidden(w)
		return
	}
	for _, raw := range strings.Split(r.Header.Get("Access-Control-Request-Headers"), ",") {
		h := strings.ToLower(strings.TrimSpace(raw))
		if h != "" && h != "authorization" && h != "content-type" && h != "x-csrf-token" {
			forbidden(w)
			return
		}
	}
	h := w.Header()
	h.Set("Access-Control-Allow-Origin", s.public.String())
	h.Set("Access-Control-Allow-Credentials", "true")
	h.Set("Access-Control-Allow-Methods", "GET, POST, DELETE, OPTIONS")
	h.Set("Access-Control-Allow-Headers", "Authorization, Content-Type, X-CSRF-Token")
	h.Set("Access-Control-Expose-Headers", "Graphite-Meter-Auth, Graphite-Meter-Auth-URL")
	h.Set("Timing-Allow-Origin", s.public.String())
	h.Add("Vary", "Origin")
	w.WriteHeader(http.StatusNoContent)
}

func allowedPreflightMethod(path, method string) bool {
	switch path {
	case "/preflight", "/probe", "/download":
		return method == http.MethodGet
	case "/upload/session", "/upload":
		return method == http.MethodPost
	case "/upload/progress":
		return method == http.MethodGet || method == http.MethodDelete
	case "/ws/ping":
		return method == http.MethodGet
	}
	return false
}

func (s *Service) authenticate(r *http.Request) (Principal, bool) {
	if raw := r.Header.Get("Authorization"); raw != "" {
		if !strings.HasPrefix(raw, "Bearer ") {
			return Principal{}, false
		}
		return s.authenticateGrant(strings.TrimPrefix(raw, "Bearer "))
	}
	c, err := r.Cookie(sessionCookie)
	if err != nil {
		return Principal{}, false
	}
	h := sha256.Sum256([]byte(c.Value))
	now := s.now()
	s.mu.Lock()
	defer s.mu.Unlock()
	sess, ok := s.sessions[h]
	if !ok || !now.Before(sess.expires) {
		if ok {
			s.deleteSessionLocked(sess)
		}
		return Principal{}, false
	}
	return Principal{Subject: sess.subject, Name: sess.name, Provider: sess.provider, Expires: sess.expires, session: sess}, true
}

func (s *Service) validRequestOrigin(r *http.Request, p Principal) bool {
	if p.Bearer {
		return r.Header.Get("Origin") == "" || r.Header.Get("Origin") == s.public.String()
	}
	if origin := r.Header.Get("Origin"); origin != "" && origin != s.public.String() {
		return false
	}
	if site := r.Header.Get("Sec-Fetch-Site"); site != "" {
		if site == "same-site" && r.Header.Get("Origin") != s.public.String() {
			return false
		}
		if site != "same-origin" && site != "same-site" && site != "none" {
			return false
		}
	}
	if p.session != nil && isMeasurementRoute(r.URL.Path) && (r.Method == http.MethodGet || r.Method == http.MethodHead) {
		if r.Header.Get("Origin") != s.public.String() && r.Header.Get("Sec-Fetch-Site") != "same-origin" {
			return false
		}
	}
	if r.URL.Path == "/ws/ping" && r.Header.Get("Origin") != s.public.String() {
		return false
	}
	if r.Method != http.MethodGet && r.Method != http.MethodHead && r.Method != http.MethodOptions {
		if r.Header.Get("Origin") != s.public.String() {
			return false
		}
		if isMeasurementRoute(r.URL.Path) && (p.session == nil || !constantEqual(p.session.csrf, r.Header.Get("X-CSRF-Token"))) {
			return false
		}
	}
	return true
}

func constantEqual(a, b string) bool {
	return len(a) > 20 && len(a) == len(b) && subtle.ConstantTimeCompare([]byte(a), []byte(b)) == 1
}

func (s *Service) createSession(subject, name, provider string, expires time.Time) (string, *session, error) {
	now := s.now()
	max := now.Add(sessionLifetime)
	if expires.IsZero() || expires.After(max) {
		expires = max
	}
	raw, err := randomToken(32)
	if err != nil {
		return "", nil, err
	}
	h := sha256.Sum256([]byte(raw))
	csrf, err := randomToken(32)
	if err != nil {
		return "", nil, err
	}
	ctx, cancel := context.WithDeadline(context.Background(), expires)
	sess := &session{hash: h, subject: subject, name: name, provider: provider, expires: expires, created: now, ctx: ctx, cancel: cancel, grants: map[[32]byte]struct{}{}, csrf: csrf}
	s.mu.Lock()
	defer s.mu.Unlock()
	s.expireLocked(now)
	var own []*session
	for _, x := range s.sessions {
		if x.subject == subject {
			own = append(own, x)
		}
	}
	sort.Slice(own, func(i, j int) bool { return own[i].created.Before(own[j].created) })
	if len(own) >= maxSubjectSessions {
		s.deleteSessionLocked(own[0])
	}
	if len(s.sessions) >= maxSessions {
		cancel()
		s.counters.capacity.Add(1)
		return "", nil, errors.New("session capacity reached")
	}
	s.sessions[h] = sess
	return raw, sess, nil
}

func (s *Service) deleteSessionLocked(sess *session) {
	delete(s.sessions, sess.hash)
	for grant := range sess.grants {
		delete(s.grants, grant)
	}
	for challenge, approval := range s.approvals {
		if approval.session == sess {
			delete(s.approvals, challenge)
		}
	}
	sess.cancel()
}
func (s *Service) expireLocked(now time.Time) {
	for _, sess := range s.sessions {
		if !now.Before(sess.expires) {
			s.deleteSessionLocked(sess)
		}
	}
}
func (s *Service) sweep(ctx context.Context) {
	t := time.NewTicker(time.Minute)
	defer t.Stop()
	for {
		select {
		case <-ctx.Done():
			return
		case <-t.C:
			s.mu.Lock()
			s.expireLocked(s.now())
			s.mu.Unlock()
		}
	}
}

func randomToken(n int) (string, error) {
	b := make([]byte, n)
	if _, err := randomBytes(b); err != nil {
		return "", err
	}
	return base64.RawURLEncoding.EncodeToString(b), nil
}

var randomBytes = rand.Read

func setCookie(w http.ResponseWriter, name, value string, expires time.Time) {
	http.SetCookie(w, &http.Cookie{Name: name, Value: value, Path: "/", Expires: expires, MaxAge: int(time.Until(expires).Seconds()), Secure: true, HttpOnly: true, SameSite: http.SameSiteStrictMode})
}
func setCSRFCookie(w http.ResponseWriter, value string, expires time.Time) {
	http.SetCookie(w, &http.Cookie{Name: csrfCookie, Value: value, Path: "/", Expires: expires, MaxAge: int(time.Until(expires).Seconds()), Secure: true, SameSite: http.SameSiteStrictMode})
}
func setTransactionCookie(w http.ResponseWriter, value string, expires time.Time) {
	http.SetCookie(w, &http.Cookie{Name: transactionCookie, Value: value, Path: "/", Expires: expires, MaxAge: int(time.Until(expires).Seconds()), Secure: true, HttpOnly: true, SameSite: http.SameSiteLaxMode})
}
func clearTransactionCookie(w http.ResponseWriter) {
	http.SetCookie(w, &http.Cookie{Name: transactionCookie, Path: "/", MaxAge: -1, Expires: time.Unix(1, 0), Secure: true, HttpOnly: true, SameSite: http.SameSiteLaxMode})
}
func clearCookie(w http.ResponseWriter, name string) {
	http.SetCookie(w, &http.Cookie{Name: name, Path: "/", MaxAge: -1, Expires: time.Unix(1, 0), Secure: true, HttpOnly: true, SameSite: http.SameSiteStrictMode})
}
func PrincipalFromContext(ctx context.Context) (Principal, bool) {
	p, ok := ctx.Value(principalKey{}).(Principal)
	return p, ok
}

func SessionEnded(ctx context.Context) bool {
	return errors.Is(context.Cause(ctx), errSessionEnded)
}

func (s *Service) csrfFailure(r *http.Request, field string) string {
	origin := r.Header.Get("Origin")
	if origin == "" {
		return "origin_missing"
	}
	if origin != s.public.String() {
		return "origin_mismatch"
	}
	c, err := r.Cookie(loginCookie)
	if err != nil {
		return "cookie_missing"
	}
	v := r.FormValue(field)
	if v == "" {
		return "token_missing"
	}
	if !constantEqual(c.Value, v) {
		return "token_mismatch"
	}
	return ""
}

func (s *Service) login(w http.ResponseWriter, r *http.Request) {
	s.loginSecurityHeaders(w.Header())
	csrf, err := randomToken(32)
	if err != nil {
		http.Error(w, "temporarily unavailable", http.StatusServiceUnavailable)
		return
	}
	setCookie(w, loginCookie, csrf, s.now().Add(10*time.Minute))
	data := loginView{
		Styles: authStyles, CSRF: csrf,
		Password:  s.cfg.Mode == "password" || s.cfg.Mode == "hybrid",
		OIDC:      s.cfg.Mode == "oidc" || s.cfg.Mode == "hybrid",
		OIDCReady: s.oidc != nil && s.oidc.ready(), Provider: s.cfg.OIDCProviderName,
		Error: r.URL.Query().Get("error") != "", Expired: r.URL.Query().Get("reason") == "expired",
		Challenge: r.URL.Query().Get("challenge"),
	}
	renderLogin(w, s.loginTemplate, data)
}

func (s *Service) passwordLogin(w http.ResponseWriter, r *http.Request) {
	securityHeaders(w.Header())
	if s.cfg.Mode != "password" && s.cfg.Mode != "hybrid" {
		http.NotFound(w, r)
		return
	}
	r.Body = http.MaxBytesReader(w, r.Body, 4096)
	if err := r.ParseForm(); err != nil {
		s.debugf("local password rejected reason=malformed_form")
		s.loginFailure(w, r)
		return
	}
	if reason := s.csrfFailure(r, "csrf"); reason != "" {
		s.debugf("local password rejected reason=csrf_" + reason)
		s.loginFailure(w, r)
		return
	}
	if !s.allowAttempt(r) {
		s.debugf("local password rejected reason=rate_limit_or_client_address")
		s.counters.throttled.Add(1)
		s.loginFailure(w, r)
		return
	}
	select {
	case s.argon <- struct{}{}:
		defer func() { <-s.argon }()
	default:
		s.debugf("local password rejected reason=verifier_busy")
		s.loginFailure(w, r)
		return
	}
	if !verifyPassword(s.passwordHash, r.FormValue("password")) {
		s.debugf("local password rejected reason=mismatch")
		s.counters.invalidPassword.Add(1)
		s.loginFailure(w, r)
		return
	}
	raw, sess, err := s.createSession("local-operator", "Local operator", "local", time.Time{})
	if err != nil {
		s.debugf("local password rejected reason=session_capacity")
		s.loginFailure(w, r)
		return
	}
	setCookie(w, sessionCookie, raw, sess.expires)
	setCSRFCookie(w, sess.csrf, sess.expires)
	s.counters.local.Add(1)
	clearCookie(w, loginCookie)
	dest := "/"
	if challenge := r.FormValue("challenge"); challenge != "" {
		dest = "/auth/cli?challenge=" + url.QueryEscape(challenge)
	}
	http.Redirect(w, r, dest, http.StatusSeeOther)
}
func (s *Service) loginFailure(w http.ResponseWriter, r *http.Request) {
	query := url.Values{"error": {"1"}}
	if challenge := r.FormValue("challenge"); validChallenge(challenge) {
		query.Set("challenge", challenge)
	}
	http.Redirect(w, r, "/login?"+query.Encode(), http.StatusSeeOther)
}
func (s *Service) allowAttempt(r *http.Request) bool {
	addr, ok := s.authClientAddress(r)
	if !ok {
		return false
	}
	key := addr.String()
	now := s.now()
	s.mu.Lock()
	defer s.mu.Unlock()
	if _, exists := s.attempts[key]; !exists && len(s.attempts) >= 2048 {
		for k, v := range s.attempts {
			v.times = recentAttempts(v.times, now)
			if len(v.times) == 0 {
				delete(s.attempts, k)
			} else {
				s.attempts[k] = v
			}
		}
		if len(s.attempts) >= 2048 {
			return false
		}
	}
	a := s.attempts[key]
	a.times = recentAttempts(a.times, now)
	if len(a.times) >= 5 {
		return false
	}
	s.globalAttempts = recentAttempts(s.globalAttempts, now)
	if len(s.globalAttempts) >= 60 {
		return false
	}
	a.times = append(a.times, now)
	s.attempts[key] = a
	s.globalAttempts = append(s.globalAttempts, now)
	return true
}

func recentAttempts(attempts []time.Time, now time.Time) []time.Time {
	cutoff := now.Add(-time.Minute)
	first := 0
	for first < len(attempts) && !attempts[first].After(cutoff) {
		first++
	}
	return attempts[first:]
}

func (s *Service) authClientAddress(r *http.Request) (netip.Addr, bool) {
	peer, err := splitRemote(r.RemoteAddr)
	if err != nil {
		return netip.Addr{}, false
	}
	peer = peer.Unmap()
	if !prefixContains(s.trusted, peer) {
		return peer, true
	}
	if r.Header.Get("Forwarded") != "" || r.Header.Get("X-Forwarded-For") != "" {
		return netip.Addr{}, false
	}
	raw := singleHeader(r.Header, "X-Real-IP")
	addr, err := netip.ParseAddr(raw)
	if err != nil {
		return netip.Addr{}, false
	}
	return addr.Unmap(), true
}

func (s *Service) sessionInfo(w http.ResponseWriter, r *http.Request) {
	securityHeaders(w.Header())
	p, ok := PrincipalFromContext(r.Context())
	if !ok {
		forbidden(w)
		return
	}
	w.Header().Set("Content-Type", "application/json")
	_ = json.NewEncoder(w).Encode(map[string]any{"name": p.Name, "provider": p.Provider, "expires": p.Expires, "csrf": p.session.csrf})
}
func (s *Service) logout(w http.ResponseWriter, r *http.Request) {
	securityHeaders(w.Header())
	r.Body = http.MaxBytesReader(w, r.Body, 4096)
	if err := r.ParseForm(); err != nil {
		forbidden(w)
		return
	}
	p, ok := PrincipalFromContext(r.Context())
	if !ok || p.session == nil || r.Header.Get("Origin") != s.public.String() || !constantEqual(p.session.csrf, r.FormValue("csrf")) {
		forbidden(w)
		return
	}
	if p.session != nil {
		s.mu.Lock()
		s.deleteSessionLocked(p.session)
		s.mu.Unlock()
	}
	s.counters.logout.Add(1)
	clearCookie(w, sessionCookie)
	clearCookie(w, loginCookie)
	clearCookie(w, csrfCookie)
	http.Redirect(w, r, "/login", http.StatusSeeOther)
}

func (s *Service) authenticateGrant(raw string) (Principal, bool) {
	decoded, err := base64.RawURLEncoding.DecodeString(raw)
	if err != nil || len(decoded) != 32 || base64.RawURLEncoding.EncodeToString(decoded) != raw {
		return Principal{}, false
	}
	h := sha256.Sum256([]byte(raw))
	now := s.now()
	s.mu.Lock()
	defer s.mu.Unlock()
	sess := s.grants[h]
	if sess != nil && now.Before(sess.expires) && sess.ctx.Err() == nil {
		return Principal{Subject: sess.subject, Name: sess.name, Provider: "cli", Expires: sess.expires, session: sess, Bearer: true}, true
	}
	return Principal{}, false
}

var _ = fmt.Sprintf

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
