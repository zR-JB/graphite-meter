package auth

import (
	"context"
	"crypto/sha256"
	"errors"
	"fmt"
	"io"
	"log"
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
	tx             map[[32]byte]oidcTransaction
}

type oidcDiscovery struct {
	provider                       *oidc.Provider
	verifier                       *oidc.IDTokenVerifier
	oauth                          oauth2.Config
	responseIssuer                 bool
	tokenURL, userinfoURL, jwksURL string
}

func newOIDCState(cfg config.AuthConfig, secret string) *oidcState {
	return &oidcState{cfg: cfg, secret: secret, tx: map[[32]byte]oidcTransaction{}}
}
func (o *oidcState) ready() bool { o.mu.RLock(); defer o.mu.RUnlock(); return o.provider != nil }

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
		return nil, errors.New("provider unavailable")
	}
	var meta struct {
		ResponseIssuer bool   `json:"authorization_response_iss_parameter_supported"`
		UserInfo       string `json:"userinfo_endpoint"`
		JWKS           string `json:"jwks_uri"`
	}
	_ = p.Claims(&meta)
	ep := p.Endpoint()
	if !validProviderURL(ep.AuthURL) || !validProviderURL(ep.TokenURL) || !validProviderURL(meta.UserInfo) || !validProviderURL(meta.JWKS) {
		return nil, errors.New("provider endpoints must use HTTPS")
	}
	ep.AuthStyle = oauth2.AuthStyleInHeader
	return &oidcDiscovery{
		provider: p, verifier: p.Verifier(&oidc.Config{ClientID: o.cfg.OIDCClientID}),
		oauth:          oauth2.Config{ClientID: o.cfg.OIDCClientID, ClientSecret: o.secret, Endpoint: ep, RedirectURL: public.String() + "/auth/oidc/callback", Scopes: []string{oidc.ScopeOpenID, "profile", "groups"}},
		responseIssuer: meta.ResponseIssuer, tokenURL: ep.TokenURL, userinfoURL: meta.UserInfo, jwksURL: meta.JWKS,
	}, nil
}

func (o *oidcState) check(ctx context.Context, public *url.URL) (*oidcDiscovery, error) {
	discovery, err := o.discover(ctx, public)
	if err != nil {
		return nil, err
	}
	for _, endpoint := range []struct {
		url       string
		requireOK bool
	}{{discovery.tokenURL, false}, {discovery.userinfoURL, false}, {discovery.jwksURL, true}} {
		if err := probeProviderEndpoint(ctx, endpoint.url, endpoint.requireOK); err != nil {
			return nil, err
		}
	}
	return discovery, nil
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
		discovery, err := o.check(ctx, public)
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

func probeProviderEndpoint(ctx context.Context, raw string, requireOK bool) error {
	probeCtx, cancel := context.WithTimeout(ctx, 10*time.Second)
	defer cancel()
	req, err := http.NewRequestWithContext(probeCtx, http.MethodGet, raw, nil)
	if err != nil {
		return errors.New("provider unavailable")
	}
	res, err := providerHTTPClient().Do(req)
	if err != nil {
		return errors.New("provider unavailable")
	}
	defer res.Body.Close()
	if !healthyProviderStatus(res.StatusCode, requireOK) {
		return errors.New("provider unavailable")
	}
	return nil
}

func healthyProviderStatus(status int, requireOK bool) bool {
	if requireOK {
		return status == http.StatusOK
	}
	if status >= http.StatusOK && status < http.StatusMultipleChoices {
		return true
	}
	switch status {
	case http.StatusBadRequest, http.StatusUnauthorized, http.StatusForbidden, http.StatusMethodNotAllowed:
		return true
	default:
		return false
	}
}

func (o *oidcState) refresh(ctx context.Context, public *url.URL) bool {
	discovery, err := o.check(ctx, public)
	o.mu.Lock()
	wasReady := o.provider != nil
	if err != nil {
		o.provider = nil
		o.verifier = nil
	} else {
		o.provider = discovery.provider
		o.verifier = discovery.verifier
		o.oauth = discovery.oauth
		o.responseIssuer = discovery.responseIssuer
	}
	o.mu.Unlock()
	if err != nil && wasReady {
		log.Printf("[gm:auth] OIDC provider unavailable")
	} else if err != nil {
		log.Printf("[gm:auth] OIDC provider retrying")
	} else if !wasReady {
		log.Printf("[gm:auth] OIDC provider recovered")
	}
	return err == nil
}

func (o *oidcState) monitor(ctx context.Context, public *url.URL) {
	ticker := time.NewTicker(time.Minute)
	defer ticker.Stop()
	for {
		select {
		case <-ctx.Done():
			return
		case <-ticker.C:
			o.mu.RLock()
			retrying := o.retrying
			o.mu.RUnlock()
			if !retrying {
				o.refresh(ctx, public)
			}
		}
	}
}

func (s *Service) oidcStart(w http.ResponseWriter, r *http.Request) {
	securityHeaders(w.Header())
	if s.oidc == nil || !s.oidc.ready() {
		s.oidcLoginFailure(w, r)
		return
	}
	r.Body = http.MaxBytesReader(w, r.Body, 4096)
	if err := r.ParseForm(); err != nil || !s.csrfOK(r, "csrf") {
		s.oidcLoginFailure(w, r)
		return
	}
	state, err := randomToken(32)
	if err != nil {
		s.oidcLoginFailure(w, r)
		return
	}
	nonce, err := randomToken(32)
	if err != nil {
		s.oidcLoginFailure(w, r)
		return
	}
	verifier := oauth2.GenerateVerifier()
	browser, err := randomToken(32)
	if err != nil {
		s.oidcLoginFailure(w, r)
		return
	}
	key := sha256.Sum256([]byte(state))
	bh := sha256.Sum256([]byte(browser))
	addr, ok := s.authClientAddress(r)
	if !ok {
		s.oidcLoginFailure(w, r)
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
		s.oidcLoginFailure(w, r)
		return
	}
	if o.provider == nil || o.verifier == nil {
		o.mu.Unlock()
		s.oidcLoginFailure(w, r)
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
		s.oidcLoginFailure(w, r)
		return
	}
	key := sha256.Sum256([]byte(state))
	cookie, err := r.Cookie(transactionCookie)
	if err != nil {
		s.oidcLoginFailure(w, r)
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
		s.oidcLoginFailure(w, r)
		return
	}
	iss := first(q["iss"])
	if (tx.responseIssuer && iss != s.cfg.OIDCIssuer) || (!tx.responseIssuer && iss != "" && iss != s.cfg.OIDCIssuer) {
		s.oidcLoginFailure(w, r)
		return
	}
	ctx, cancel := context.WithTimeout(r.Context(), 15*time.Second)
	defer cancel()
	ctx = oidc.ClientContext(ctx, providerHTTPClient())
	token, err := tx.oauth.Exchange(ctx, code, oauth2.VerifierOption(tx.verifier))
	if err != nil {
		s.oidcLoginFailure(w, r)
		return
	}
	rawID, ok := token.Extra("id_token").(string)
	if !ok {
		s.oidcLoginFailure(w, r)
		return
	}
	idToken, err := tx.idVerifier.Verify(ctx, rawID)
	if err != nil {
		s.oidcLoginFailure(w, r)
		return
	}
	var idClaims struct {
		Nonce    string `json:"nonce"`
		Name     string `json:"name"`
		Username string `json:"preferred_username"`
		AtHash   string `json:"at_hash"`
	}
	if err := idToken.Claims(&idClaims); err != nil || idClaims.Nonce != tx.nonce {
		s.oidcLoginFailure(w, r)
		return
	}
	if idClaims.AtHash != "" {
		if err := idToken.VerifyAccessToken(token.AccessToken); err != nil {
			s.oidcLoginFailure(w, r)
			return
		}
	}
	ui, err := tx.provider.UserInfo(ctx, oauth2.StaticTokenSource(token))
	if err != nil || ui.Subject != idToken.Subject {
		s.oidcLoginFailure(w, r)
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
		s.oidcLoginFailure(w, r)
		return
	}
	if !validSubject(idToken.Subject) {
		s.oidcLoginFailure(w, r)
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
		s.oidcLoginFailure(w, r)
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

func (s *Service) oidcLoginFailure(w http.ResponseWriter, r *http.Request) {
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

var _ = fmt.Sprintf
