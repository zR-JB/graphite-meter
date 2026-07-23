package auth

import (
	"context"
	"crypto/sha256"
	"crypto/x509"
	"errors"
	"io"
	"log"
	"net"
	"net/http"
	"net/url"
	"strings"
	"sync"
	"time"

	"github.com/coreos/go-oidc/v3/oidc"
	"github.com/zR-JB/graphite-meter/go/internal/config"
	"golang.org/x/oauth2"
)

type oidcTransaction struct {
	state, nonce, verifier string
	browser                [32]byte
	expires                time.Time
	client                 string
	cliChallenge           string
	provider               *oidc.Provider
	idVerifier             *oidc.IDTokenVerifier
	oauth                  oauth2.Config
	responseIssuer         bool
}

type oidcState struct {
	cfg            config.AuthConfig
	secret         string
	mu             sync.RWMutex
	provider       *oidc.Provider
	verifier       *oidc.IDTokenVerifier
	oauth          oauth2.Config
	responseIssuer bool
	retrying       bool
	verbose        bool
	tx             map[[32]byte]oidcTransaction
}

type oidcDiscovery struct {
	provider       *oidc.Provider
	verifier       *oidc.IDTokenVerifier
	oauth          oauth2.Config
	responseIssuer bool
}

func newOIDCState(cfg config.AuthConfig, secret string, verbose bool) *oidcState {
	return &oidcState{cfg: cfg, secret: secret, tx: map[[32]byte]oidcTransaction{}, verbose: verbose}
}
func (o *oidcState) ready() bool { o.mu.RLock(); defer o.mu.RUnlock(); return o.provider != nil }
func (o *oidcState) authorizationOrigin() string {
	o.mu.RLock()
	raw := o.oauth.Endpoint.AuthURL
	o.mu.RUnlock()
	u, err := url.Parse(raw)
	if err != nil || !validProviderURL(raw) {
		return ""
	}
	return u.Scheme + "://" + u.Host
}

type limitTransport struct{ base http.RoundTripper }

func (t limitTransport) RoundTrip(r *http.Request) (*http.Response, error) {
	resp, err := t.base.RoundTrip(r)
	if err == nil && resp.Body != nil {
		resp.Body = struct {
			io.Reader
			io.Closer
		}{io.LimitReader(resp.Body, 1<<20), resp.Body}
	}
	return resp, err
}

func providerHTTPClient() *http.Client {
	return &http.Client{Timeout: 10 * time.Second, Transport: limitTransport{base: http.DefaultTransport}, CheckRedirect: func(*http.Request, []*http.Request) error {
		return http.ErrUseLastResponse
	}}
}

func (o *oidcState) discover(ctx context.Context, public *url.URL) (*oidcDiscovery, error) {
	client := providerHTTPClient()
	ctx = oidc.ClientContext(ctx, client)
	p, err := oidc.NewProvider(ctx, o.cfg.OIDCIssuer)
	if err != nil {
		failure := classifyDiscoveryFailure(err)
		o.debugln("OIDC discovery failed reason=" + failure.reason)
		return nil, failure
	}
	var meta struct {
		ResponseIssuer bool   `json:"authorization_response_iss_parameter_supported"`
		UserInfo       string `json:"userinfo_endpoint"`
		JWKS           string `json:"jwks_uri"`
	}
	_ = p.Claims(&meta)
	ep := p.Endpoint()
	if !validProviderURL(ep.AuthURL) || !validProviderURL(ep.TokenURL) || !validProviderURL(meta.UserInfo) || !validProviderURL(meta.JWKS) {
		o.debugln("OIDC discovery failed reason=invalid_endpoint_metadata")
		return nil, &discoveryFailure{reason: "invalid_endpoint_metadata"}
	}
	ep.AuthStyle = oauth2.AuthStyleInHeader
	return &oidcDiscovery{
		provider: p, verifier: p.Verifier(&oidc.Config{ClientID: o.cfg.OIDCClientID}),
		oauth:          oauth2.Config{ClientID: o.cfg.OIDCClientID, ClientSecret: o.secret, Endpoint: ep, RedirectURL: public.String() + "/auth/oidc/callback", Scopes: []string{oidc.ScopeOpenID, "profile", "groups"}},
		responseIssuer: meta.ResponseIssuer,
	}, nil
}

type discoveryFailure struct{ reason string }

func (e *discoveryFailure) Error() string { return "provider unavailable" }

func classifyDiscoveryFailure(err error) *discoveryFailure {
	reason := "discovery_response"
	var issuer *oidc.IssuerMismatchError
	var dns *net.DNSError
	var unknownAuthority x509.UnknownAuthorityError
	var hostname x509.HostnameError
	switch {
	case errors.Is(err, context.DeadlineExceeded):
		reason = "timeout"
	case errors.Is(err, context.Canceled):
		reason = "cancelled"
	case errors.As(err, &issuer):
		reason = "issuer_mismatch"
	case errors.As(err, &dns):
		reason = "dns"
	case errors.As(err, &unknownAuthority), errors.As(err, &hostname):
		reason = "tls_verification"
	default:
		var op *net.OpError
		if errors.As(err, &op) {
			reason = "connection"
		}
	}
	return &discoveryFailure{reason: reason}
}

func (o *oidcState) debugln(message string) {
	if o.verbose {
		log.Printf("[gm:auth:debug] %s", message)
	}
}

func (o *oidcState) install(discovery *oidcDiscovery) {
	o.mu.Lock()
	o.provider = discovery.provider
	o.verifier = discovery.verifier
	o.oauth = discovery.oauth
	o.responseIssuer = discovery.responseIssuer
	o.mu.Unlock()
}
func validProviderURL(raw string) bool {
	u, err := url.Parse(raw)
	return err == nil && u.Scheme == "https" && u.Hostname() != "" && u.User == nil
}
func (o *oidcState) startRetry(ctx context.Context, public *url.URL) {
	o.mu.Lock()
	if o.retrying {
		o.mu.Unlock()
		return
	}
	o.retrying = true
	o.mu.Unlock()
	go o.retryDiscovery(ctx, public)
}

func (o *oidcState) retryDiscovery(ctx context.Context, public *url.URL) {
	delay := time.Second
	unavailableLogged := false
	for {
		discovery, err := o.discover(ctx, public)
		if err == nil {
			o.install(discovery)
			o.mu.Lock()
			o.retrying = false
			o.mu.Unlock()
			log.Printf("[gm:auth] OIDC provider ready")
			return
		}
		if !unavailableLogged {
			log.Printf("[gm:auth] OIDC provider unavailable; local password remains available")
			unavailableLogged = true
		} else {
			log.Printf("[gm:auth] OIDC provider retrying")
		}
		select {
		case <-ctx.Done():
			o.mu.Lock()
			o.retrying = false
			o.mu.Unlock()
			return
		case <-time.After(delay):
		}
		if delay < time.Minute {
			delay *= 2
			if delay > time.Minute {
				delay = time.Minute
			}
		}
	}
}

func (s *Service) oidcStart(w http.ResponseWriter, r *http.Request) {
	s.loginSecurityHeaders(w.Header())
	if s.oidc == nil || !s.oidc.ready() {
		s.oidcLoginFailure(w, r, "provider_not_ready")
		return
	}
	r.Body = http.MaxBytesReader(w, r.Body, 4096)
	if err := r.ParseForm(); err != nil {
		s.oidcLoginFailure(w, r, "start_request_malformed_form")
		return
	}
	if reason := s.csrfFailure(r, "csrf"); reason != "" {
		s.oidcLoginFailure(w, r, "start_request_csrf_"+reason)
		return
	}
	state, err := randomToken(32)
	if err != nil {
		s.oidcLoginFailure(w, r, "state_generation")
		return
	}
	nonce, err := randomToken(32)
	if err != nil {
		s.oidcLoginFailure(w, r, "nonce_generation")
		return
	}
	verifier := oauth2.GenerateVerifier()
	browser, err := randomToken(32)
	if err != nil {
		s.oidcLoginFailure(w, r, "browser_binding_generation")
		return
	}
	key := sha256.Sum256([]byte(state))
	bh := sha256.Sum256([]byte(browser))
	addr, ok := s.authClientAddress(r)
	if !ok {
		s.oidcLoginFailure(w, r, "client_address")
		return
	}
	client := addr.String()
	tx := oidcTransaction{state: state, nonce: nonce, verifier: verifier, browser: bh, expires: s.now().Add(10 * time.Minute), client: client, cliChallenge: r.FormValue("challenge")}
	o := s.oidc
	o.mu.Lock()
	now := s.now()
	for k, v := range o.tx {
		if !now.Before(v.expires) {
			delete(o.tx, k)
		}
	}
	perClient := 0
	for _, v := range o.tx {
		if v.client == client {
			perClient++
		}
	}
	if len(o.tx) >= 256 || perClient >= 8 {
		o.mu.Unlock()
		s.counters.capacity.Add(1)
		s.oidcLoginFailure(w, r, "transaction_capacity")
		return
	}
	if o.provider == nil || o.verifier == nil {
		o.mu.Unlock()
		s.oidcLoginFailure(w, r, "provider_not_ready")
		return
	}
	oauthCfg := o.oauth
	tx.oauth = oauthCfg
	tx.provider = o.provider
	tx.idVerifier = o.verifier
	tx.responseIssuer = o.responseIssuer
	o.tx[key] = tx
	o.mu.Unlock()
	setTransactionCookie(w, browser, tx.expires)
	location := oauthCfg.AuthCodeURL(state, oauth2.S256ChallengeOption(verifier), oauth2.SetAuthURLParam("nonce", nonce))
	http.Redirect(w, r, location, http.StatusSeeOther)
}

func exactlyOne(q url.Values, key string) (string, bool) {
	v, ok := q[key]
	return first(v), ok && len(v) == 1 && v[0] != ""
}
func first(v []string) string {
	if len(v) == 0 {
		return ""
	}
	return v[0]
}

func (s *Service) oidcCallback(w http.ResponseWriter, r *http.Request) {
	securityHeaders(w.Header())
	q := r.URL.Query()
	code, cok := exactlyOne(q, "code")
	state, sok := exactlyOne(q, "state")
	if !cok || !sok || len(q["error"]) > 0 || len(q["iss"]) > 1 {
		s.oidcLoginFailure(w, r, "callback_parameters")
		return
	}
	key := sha256.Sum256([]byte(state))
	cookie, err := r.Cookie(transactionCookie)
	if err != nil {
		s.oidcLoginFailure(w, r, "transaction_cookie")
		return
	}
	bh := sha256.Sum256([]byte(cookie.Value))
	o := s.oidc
	o.mu.Lock()
	tx, ok := o.tx[key]
	if ok {
		delete(o.tx, key)
	}
	o.mu.Unlock()
	clearTransactionCookie(w)
	if ok && validChallenge(tx.cliChallenge) {
		values := r.URL.Query()
		values.Set("challenge", tx.cliChallenge)
		r.URL.RawQuery = values.Encode()
	}
	if !ok || !s.now().Before(tx.expires) || tx.browser != bh || tx.state != state || tx.provider == nil || tx.idVerifier == nil {
		s.counters.replayExpiry.Add(1)
		s.oidcLoginFailure(w, r, "transaction_replay_or_expiry")
		return
	}
	iss := first(q["iss"])
	if (tx.responseIssuer && iss != s.cfg.OIDCIssuer) || (!tx.responseIssuer && iss != "" && iss != s.cfg.OIDCIssuer) {
		s.oidcLoginFailure(w, r, "response_issuer")
		return
	}
	ctx, cancel := context.WithTimeout(r.Context(), 15*time.Second)
	defer cancel()
	ctx = oidc.ClientContext(ctx, providerHTTPClient())
	token, err := tx.oauth.Exchange(ctx, code, oauth2.VerifierOption(tx.verifier))
	if err != nil {
		s.oidcLoginFailure(w, r, "token_exchange")
		return
	}
	rawID, ok := token.Extra("id_token").(string)
	if !ok {
		s.oidcLoginFailure(w, r, "missing_id_token")
		return
	}
	idToken, err := tx.idVerifier.Verify(ctx, rawID)
	if err != nil {
		s.oidcLoginFailure(w, r, "id_token_verification")
		return
	}
	var idClaims struct {
		Nonce    string `json:"nonce"`
		Name     string `json:"name"`
		Username string `json:"preferred_username"`
		AtHash   string `json:"at_hash"`
	}
	if err := idToken.Claims(&idClaims); err != nil || idClaims.Nonce != tx.nonce {
		s.oidcLoginFailure(w, r, "id_token_claims_or_nonce")
		return
	}
	if idClaims.AtHash != "" {
		if err := idToken.VerifyAccessToken(token.AccessToken); err != nil {
			s.oidcLoginFailure(w, r, "access_token_hash")
			return
		}
	}
	ui, err := tx.provider.UserInfo(ctx, oauth2.StaticTokenSource(token))
	if err != nil || ui.Subject != idToken.Subject {
		s.oidcLoginFailure(w, r, "userinfo_or_subject")
		return
	}
	var claims struct {
		Name     string   `json:"name"`
		Username string   `json:"preferred_username"`
		Groups   []string `json:"groups"`
	}
	if err := ui.Claims(&claims); err != nil || !allowedGroup(claims.Groups, s.cfg.OIDCAllowedGroups) {
		if err == nil {
			s.counters.groupDenial.Add(1)
		}
		s.oidcLoginFailure(w, r, "userinfo_claims_or_group")
		return
	}
	if !validSubject(idToken.Subject) {
		s.oidcLoginFailure(w, r, "invalid_subject")
		return
	}
	name := claims.Name
	if name == "" {
		name = claims.Username
	}
	if name == "" {
		name = idClaims.Name
	}
	if name == "" {
		name = idClaims.Username
	}
	if name == "" {
		name = idToken.Subject
	}
	name = safeDisplayName(name)
	raw, sess, err := s.createSession("oidc:"+idToken.Subject, name, s.cfg.OIDCProviderName, idToken.Expiry)
	if err != nil {
		s.oidcLoginFailure(w, r, "session_capacity")
		return
	}
	setCookie(w, sessionCookie, raw, sess.expires)
	setCSRFCookie(w, sess.csrf, sess.expires)
	s.counters.oidc.Add(1)
	clearCookie(w, loginCookie)
	dest := "/"
	if tx.cliChallenge != "" {
		dest = "/auth/cli?challenge=" + url.QueryEscape(tx.cliChallenge)
	}
	http.Redirect(w, r, dest, http.StatusSeeOther)
}

func (s *Service) oidcLoginFailure(w http.ResponseWriter, r *http.Request, reason string) {
	s.debugln("OIDC login rejected reason=" + reason)
	s.counters.oidcFailure.Add(1)
	s.loginFailure(w, r)
}

func validSubject(v string) bool {
	if len(v) == 0 || len(v) > 256 {
		return false
	}
	for _, r := range v {
		if r < ' ' || r == 0x7f {
			return false
		}
	}
	return true
}
func safeDisplayName(v string) string {
	var b strings.Builder
	for _, r := range v {
		if r < ' ' || r == 0x7f {
			continue
		}
		if b.Len()+len(string(r)) > 256 {
			break
		}
		b.WriteRune(r)
	}
	if b.Len() == 0 {
		return "OIDC user"
	}
	return b.String()
}

func allowedGroup(actual, allowed []string) bool {
	for _, a := range actual {
		for _, want := range allowed {
			if a == want {
				return true
			}
		}
	}
	return false
}
