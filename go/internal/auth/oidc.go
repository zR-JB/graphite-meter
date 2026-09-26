package auth

import (
	"cmp"
	"context"
	"crypto/sha256"
	"errors"
	"io"
	"log"
	"maps"
	"net/http"
	"net/url"
	"slices"
	"strings"
	"sync"
	"sync/atomic"
	"time"
	"unicode/utf8"

	"github.com/coreos/go-oidc/v3/oidc"
	"github.com/zR-JB/graphite-meter/go/internal/config"
	"github.com/zR-JB/graphite-meter/go/internal/transport"
	"golang.org/x/oauth2"
)

const (
	maxOIDCTransactions       = 256
	maxClientOIDCTransactions = 8
	oidcTransactionLifetime   = 10 * time.Minute
)

type oidcTransaction struct {
	state, nonce, verifier string
	browser                [32]byte
	expires                time.Time
	client                 string
	cliChallenge           string
	discovery              *oidcDiscovery // the provider as discovered when the transaction started
	prior                  [32]byte
}

type oidcState struct {
	cfg        config.AuthConfig
	secret     string
	verbose    bool
	discovered atomic.Pointer[oidcDiscovery] // nil until discovery succeeds
	mu         sync.Mutex
	tx         map[[32]byte]oidcTransaction
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

func (o *oidcState) ready() bool { return o.discovered.Load() != nil }

func (o *oidcState) authorizationOrigin() string {
	d := o.discovered.Load()
	if d == nil || !validProviderURL(d.oauth.Endpoint.AuthURL) {
		return ""
	}
	u, _ := url.Parse(d.oauth.Endpoint.AuthURL)
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
	return &http.Client{
		Timeout:       10 * time.Second,
		Transport:     limitTransport{base: http.DefaultTransport},
		CheckRedirect: func(*http.Request, []*http.Request) error { return http.ErrUseLastResponse },
	}
}

func (o *oidcState) discover(ctx context.Context, public *url.URL) (*oidcDiscovery, error) {
	ctx = oidc.ClientContext(ctx, providerHTTPClient())
	p, err := oidc.NewProvider(ctx, o.cfg.OIDCIssuer)
	if err != nil {
		debugln(o.verbose, "OIDC discovery failed: "+err.Error())
		return nil, err
	}
	var meta struct {
		ResponseIssuer bool   `json:"authorization_response_iss_parameter_supported"`
		UserInfo       string `json:"userinfo_endpoint"`
		JWKS           string `json:"jwks_uri"`
	}
	_ = p.Claims(&meta)
	ep := p.Endpoint()
	if !validProviderURL(ep.AuthURL) || !validProviderURL(ep.TokenURL) || !validProviderURL(meta.UserInfo) ||
		!validProviderURL(meta.JWKS) {
		return nil, errors.New("provider metadata names a non-HTTPS endpoint")
	}
	ep.AuthStyle = oauth2.AuthStyleInHeader
	return &oidcDiscovery{
		provider: p,
		verifier: p.Verifier(&oidc.Config{ClientID: o.cfg.OIDCClientID}),
		oauth: oauth2.Config{
			ClientID: o.cfg.OIDCClientID, ClientSecret: o.secret, Endpoint: ep,
			RedirectURL: public.String() + "/auth/oidc/callback",
			Scopes:      []string{oidc.ScopeOpenID, "profile", "groups"},
		},
		responseIssuer: meta.ResponseIssuer,
	}, nil
}

func validProviderURL(raw string) bool {
	u, err := url.Parse(raw)
	return err == nil && u.Scheme == "https" && u.Hostname() != "" && u.User == nil
}

func (o *oidcState) retryDiscovery(ctx context.Context, public *url.URL) {
	delay := time.Second
	for attempt := 0; ; attempt++ {
		discovery, err := o.discover(ctx, public)
		if err == nil {
			o.discovered.Store(discovery)
			log.Printf("[gm:auth] OIDC provider ready")
			return
		}
		if attempt == 0 {
			log.Printf("[gm:auth] OIDC provider unavailable; local password remains available")
		} else {
			log.Printf("[gm:auth] OIDC provider retrying")
		}
		select {
		case <-ctx.Done():
			return
		case <-time.After(delay):
		}
		delay = min(delay*2, time.Minute)
	}
}

func (s *Service) oidcStart(w http.ResponseWriter, r *http.Request) {
	s.loginSecurityHeaders(w.Header())
	if s.oidc == nil || !s.oidc.ready() {
		s.oidcLoginFailure(w, r, reasonProviderNotReady)
		return
	}
	r.Body = http.MaxBytesReader(w, r.Body, 4096)
	if err := r.ParseForm(); err != nil {
		s.oidcLoginFailure(w, r, reasonFormMalformed)
		return
	}
	if why, ok := s.checkCSRF(r, "csrf"); !ok {
		s.oidcLoginFailure(w, r, why)
		return
	}
	addr, ok := s.authClientAddress(r)
	if !ok {
		s.oidcLoginFailure(w, r, reasonClientAddress)
		return
	}
	browser := randomToken(32)
	tx := oidcTransaction{
		state: randomToken(32), nonce: randomToken(32), verifier: oauth2.GenerateVerifier(),
		browser: sha256.Sum256([]byte(browser)), expires: time.Now().Add(oidcTransactionLifetime),
		client: transport.AddressBucket(addr), cliChallenge: challengeOrEmpty(r.FormValue("challenge")),
	}
	if c := uniqueCookie(r, sessionCookie); c != nil {
		tx.prior = sha256.Sum256([]byte(c.Value))
	}
	o := s.oidc
	o.mu.Lock()
	now := time.Now()
	maps.DeleteFunc(o.tx, func(_ [32]byte, v oidcTransaction) bool { return !now.Before(v.expires) })
	perClient := 0
	for v := range maps.Values(o.tx) {
		if v.client == tx.client {
			perClient++
		}
	}
	global := len(o.tx) >= maxOIDCTransactions
	if global || perClient >= maxClientOIDCTransactions {
		o.mu.Unlock()
		s.count(countCapacity)
		if global {
			s.mu.Lock()
			s.noteCeilingLocked("oidc-transaction", now)
			s.mu.Unlock()
		}
		s.oidcLoginFailure(w, r, reasonTransactionCapacity)
		return
	}
	tx.discovery = o.discovered.Load()
	o.tx[sha256.Sum256([]byte(tx.state))] = tx
	o.mu.Unlock()
	setCookie(w, transactionCookie, browser, tx.expires, http.SameSiteLaxMode)
	location := tx.discovery.oauth.AuthCodeURL(tx.state,
		oauth2.S256ChallengeOption(tx.verifier), oauth2.SetAuthURLParam("nonce", tx.nonce))
	http.Redirect(w, r, location, http.StatusSeeOther)
}

func exactlyOne(q url.Values, key string) (string, bool) {
	v, ok := q[key]
	if !ok || len(v) != 1 || v[0] == "" {
		return "", false
	}
	return v[0], true
}

func validAuthCode(v string) bool {
	return v != "" && !strings.ContainsFunc(v, func(r rune) bool { return r < 0x20 || r > 0x7e })
}

func (s *Service) oidcCallback(w http.ResponseWriter, r *http.Request) {
	securityHeaders(w.Header())
	tx, code, why := s.resolveOIDCTransaction(w, r)
	if why != "" {
		s.oidcLoginFailure(w, r, why)
		return
	}
	ctx, cancel := context.WithTimeout(r.Context(), 15*time.Second)
	defer cancel()
	subject, name, why := s.verifyOIDCUser(oidc.ClientContext(ctx, providerHTTPClient()), tx, code)
	if why != "" {
		s.oidcLoginFailure(w, r, why)
		return
	}
	raw, sess, err := s.createSession("oidc:"+subject, name, s.cfg.OIDCProviderName)
	if err != nil {
		s.oidcLoginFailure(w, r, reasonSessionCapacity)
		return
	}
	s.revokeSessionHash(tx.prior, sess)
	issueSessionCookies(w, raw, sess)
	s.count(countOIDC)
	render(w, continueTemplate, map[string]any{"Styles": authStyles, "Challenge": challengeOrEmpty(tx.cliChallenge)})
}

func (s *Service) resolveOIDCTransaction(w http.ResponseWriter, r *http.Request) (oidcTransaction, string, reason) {
	q := r.URL.Query()
	code, cok := exactlyOne(q, "code")
	state, sok := exactlyOne(q, "state")
	if !cok || !sok || !validAuthCode(code) || len(q["error"]) > 0 || len(q["iss"]) > 1 {
		return oidcTransaction{}, "", reasonCallbackParameters
	}
	cookie := uniqueCookie(r, transactionCookie)
	if cookie == nil {
		return oidcTransaction{}, "", reasonTransactionCookie
	}
	key := sha256.Sum256([]byte(state))
	o := s.oidc
	o.mu.Lock()
	tx, ok := o.tx[key]
	delete(o.tx, key)
	o.mu.Unlock()
	clearCookie(w, transactionCookie, http.SameSiteLaxMode)
	if ok && validChallenge(tx.cliChallenge) {
		q.Set("challenge", tx.cliChallenge)
		r.URL.RawQuery = q.Encode()
	}
	if !ok || !time.Now().Before(tx.expires) || tx.browser != sha256.Sum256([]byte(cookie.Value)) ||
		tx.state != state || tx.discovery == nil {
		s.count(countReplayExpiry)
		return oidcTransaction{}, "", reasonTransactionReplay
	}
	if iss, _ := exactlyOne(q, "iss"); iss != s.cfg.OIDCIssuer && (tx.discovery.responseIssuer || iss != "") {
		return oidcTransaction{}, "", reasonResponseIssuer
	}
	if !s.allowExchange(r) {
		return oidcTransaction{}, "", reasonExchangeRateLimited
	}
	return tx, code, ""
}

func (s *Service) verifyOIDCUser(ctx context.Context, tx oidcTransaction, code string) (string, string, reason) {
	token, err := tx.discovery.oauth.Exchange(ctx, code, oauth2.VerifierOption(tx.verifier))
	if err != nil {
		return "", "", reasonTokenExchange
	}
	rawID, ok := token.Extra("id_token").(string)
	if !ok {
		return "", "", reasonMissingIDToken
	}
	idToken, err := tx.discovery.verifier.Verify(ctx, rawID)
	if err != nil {
		return "", "", reasonIDTokenVerification
	}
	var idClaims struct {
		Nonce    string `json:"nonce"`
		Name     string `json:"name"`
		Username string `json:"preferred_username"`
		AtHash   string `json:"at_hash"`
	}
	if err := idToken.Claims(&idClaims); err != nil || idClaims.Nonce != tx.nonce {
		return "", "", reasonIDTokenClaimsOrNonce
	}
	if idClaims.AtHash != "" && idToken.VerifyAccessToken(token.AccessToken) != nil {
		return "", "", reasonAccessTokenHash
	}
	userInfo, err := tx.discovery.provider.UserInfo(ctx, oauth2.StaticTokenSource(token))
	if err != nil || userInfo.Subject != idToken.Subject {
		return "", "", reasonUserInfoOrSubject
	}
	var claims struct {
		Name     string   `json:"name"`
		Username string   `json:"preferred_username"`
		Groups   []string `json:"groups"`
	}
	if err := userInfo.Claims(&claims); err != nil {
		return "", "", reasonUserInfoClaimsOrGroup
	}
	if !slices.ContainsFunc(claims.Groups, func(g string) bool { return slices.Contains(s.cfg.OIDCAllowedGroups, g) }) {
		s.count(countGroupDenial)
		return "", "", reasonUserInfoClaimsOrGroup
	}
	if !validSubject(idToken.Subject) {
		return "", "", reasonInvalidSubject
	}
	name := cmp.Or(claims.Name, claims.Username, idClaims.Name, idClaims.Username, idToken.Subject)
	return idToken.Subject, safeDisplayName(name), ""
}

func (s *Service) oidcLoginFailure(w http.ResponseWriter, r *http.Request, why reason) {
	s.count(countOIDCFailure)
	s.loginRejected(w, r, why)
}

func validSubject(v string) bool {
	return len(v) > 0 && len(v) <= 256 && !strings.ContainsFunc(v, func(r rune) bool { return r < ' ' || r == 0x7f })
}

func safeDisplayName(v string) string {
	v = strings.Map(func(r rune) rune {
		if r < ' ' || r == 0x7f {
			return -1
		}
		return r
	}, v)
	if len(v) > 256 {
		v = v[:256]
		for !utf8.ValidString(v) {
			v = v[:len(v)-1]
		}
	}
	if v == "" {
		return "OIDC user"
	}
	return v
}
