package goclient

import (
	"bytes"
	"context"
	"crypto/rand"
	"crypto/sha256"
	"encoding/base32"
	"encoding/base64"
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"net/url"
	"os/exec"
	"runtime"
	"strings"
	"time"
)

// AuthRequiredError reports a server that refuses the request until the
// operator approves a grant at URL.
type AuthRequiredError struct{ URL string }

func (e *AuthRequiredError) Error() string { return "authentication required at " + e.URL }

func authResponseError(res *http.Response) error {
	if res == nil {
		return nil
	}
	if res.StatusCode == http.StatusForbidden && res.Header.Get("Graphite-Meter-Auth") == "required" {
		return &AuthRequiredError{URL: res.Header.Get("Graphite-Meter-Auth-URL")}
	}
	return nil
}

// ClassifyAuthFailure re-checks the server when a run fails, turning runErr
// into an AuthRequiredError if the grant is revoked. Revocation cuts a transfer
// short as a bare stream error, so the cause needs a fresh request.
func ClassifyAuthFailure(ctx context.Context, cfg Config, runErr error) error {
	if runErr == nil || ctx.Err() != nil || cfg.authToken() == "" {
		return runErr
	}
	checkCtx, cancel := context.WithTimeout(context.Background(), 3*time.Second)
	defer cancel()
	tr := baseTransport(cfg)
	defer tr.CloseIdleConnections()
	return classifyAuthFailure(checkCtx, authenticatedClient(cfg, tr), cfg.BaseURL, runErr)
}

func classifyAuthFailure(ctx context.Context, client *http.Client, baseURL string, runErr error) error {
	target, err := url.JoinPath(baseURL, "/preflight")
	if err != nil {
		return runErr
	}
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, target, nil)
	if err != nil {
		return runErr
	}
	res, err := client.Do(req)
	if err != nil {
		return runErr
	}
	defer res.Body.Close()
	if authErr := authResponseError(res); authErr != nil {
		return authErr
	}
	return runErr
}

type PendingAuthorization struct {
	BrowserURL, Code   string
	Origin             string
	verifier, tokenURL string
	client             *http.Client
	close              func()
}

// BeginAuthorization starts a PKCE exchange against authURL and opens the
// operator's browser at it. The grant only ever travels over verified HTTPS, so
// an unparseable, plaintext or -insecure server URL is refused outright.
func BeginAuthorization(cfg Config, authURL string) (*PendingAuthorization, error) {
	if cfg.InsecureSkipTLSVerify {
		return nil, errors.New("authenticated operation refuses -insecure")
	}
	base, err := url.Parse(cfg.BaseURL)
	if err != nil || base.Scheme != "https" {
		return nil, errors.New("authenticated operation requires an HTTPS -url")
	}
	issuingOrigin, err := canonicalOrigin(cfg.BaseURL)
	if err != nil {
		return nil, errors.New("authenticated operation requires an HTTPS -url")
	}
	login, err := authenticationLoginURL(base, authURL)
	if err != nil {
		return nil, err
	}
	b := make([]byte, 32)
	if _, err := rand.Read(b); err != nil {
		return nil, err
	}
	verifier := base64.RawURLEncoding.EncodeToString(b)
	sum := sha256.Sum256([]byte(verifier))
	challenge := base64.RawURLEncoding.EncodeToString(sum[:])
	login.Path = "/auth/cli"
	q := login.Query()
	q.Set("challenge", challenge)
	login.RawQuery = q.Encode()
	token := *login
	token.Path = "/auth/cli/token"
	token.RawQuery = ""
	code := base32.StdEncoding.WithPadding(base32.NoPadding).EncodeToString(sum[:5])
	tr := baseTransport(cfg)
	client := authenticatedClient(cfg, tr)
	client.CheckRedirect = func(*http.Request, []*http.Request) error {
		return errors.New("authentication endpoints must not redirect")
	}
	p := &PendingAuthorization{BrowserURL: login.String(), Code: code, Origin: issuingOrigin, verifier: verifier, tokenURL: token.String(), client: client, close: tr.CloseIdleConnections}
	openBrowser(p.BrowserURL)
	return p, nil
}

func authenticationLoginURL(base *url.URL, raw string) (*url.URL, error) {
	login, err := url.Parse(raw)
	if err != nil || login.Scheme != "https" || !strings.EqualFold(login.Hostname(), base.Hostname()) || login.Path != "/login" || login.User != nil || login.RawQuery != "" || login.ForceQuery || login.Fragment != "" {
		return nil, errors.New("server returned an invalid authentication URL")
	}
	return login, nil
}

// Poll waits for the browser approval, returning the grant. Transport errors
// retry: the server may restart while the operator is still in the browser.
// lastTransportErr names the cause when the deadline arrives.
func (p *PendingAuthorization) Poll(ctx context.Context) (string, error) {
	defer p.close()
	ticker := time.NewTicker(time.Second)
	defer ticker.Stop()
	var lastTransportErr error
	for {
		body, _ := json.Marshal(map[string]string{"verifier": p.verifier})
		req, err := http.NewRequestWithContext(ctx, http.MethodPost, p.tokenURL, bytes.NewReader(body))
		if err != nil {
			return "", err
		}
		req.Header.Set("Content-Type", "application/json")
		res, err := p.client.Do(req)
		lastTransportErr = err
		if err == nil {
			var out struct {
				Token string `json:"token"`
			}
			// A body that fails to decode leaves Token empty, which the checks
			// below already treat as "not approved yet".
			_ = json.NewDecoder(res.Body).Decode(&out)
			_ = res.Body.Close()
			if res.StatusCode == http.StatusOK && out.Token != "" {
				return out.Token, nil
			}
			if res.StatusCode != http.StatusAccepted {
				return "", fmt.Errorf("client approval returned HTTP %d", res.StatusCode)
			}
		}
		select {
		case <-ctx.Done():
			if errors.Is(ctx.Err(), context.Canceled) {
				return "", ctx.Err()
			}
			if lastTransportErr != nil {
				return "", fmt.Errorf("server unreachable while waiting for browser approval: %w", lastTransportErr)
			}
			return "", errors.New("browser approval timed out")
		case <-ticker.C:
		}
	}
}

// openBrowser is best-effort: the caller also surfaces BrowserURL and Code, so
// a headless or misconfigured desktop costs the operator nothing.
func openBrowser(target string) {
	var cmd *exec.Cmd
	switch runtime.GOOS {
	case "darwin":
		cmd = exec.Command("open", target)
	case "windows":
		cmd = exec.Command("rundll32", "url.dll,FileProtocolHandler", target)
	default:
		cmd = exec.Command("xdg-open", target)
	}
	if cmd.Start() == nil && cmd.Process != nil {
		_ = cmd.Process.Release()
	}
}
