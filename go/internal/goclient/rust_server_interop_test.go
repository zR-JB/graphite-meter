package goclient

import (
	"context"
	"errors"
	"io"
	"net/http"
	"net/url"
	"os"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

// The Rust interop harness supplies a trusted temporary TLS root and endpoint.
// This exercises the actual native Go measurement engine after browser approval.
func TestRustServerNativeApproval(t *testing.T) {
	public := os.Getenv("GM_RUST_INTEROP_URL")
	if public == "" {
		t.Skip("requires the Rust server interop harness")
	}
	ctx, cancel := context.WithTimeout(t.Context(), 35*time.Second)
	defer cancel()
	cfg := DefaultConfig()
	cfg.BaseURL = public
	cfg.ThroughputProtocol = "http3"
	cfg.ThroughputTransport = wire.TransportWebTransport
	cfg.LatencyTransport = wire.TransportWebTransport
	cfg.Stages = StageSet{Latency: true, Download: true, Upload: true, Bidirectional: true}
	cfg.Warmup = 100 * time.Millisecond
	cfg.LatencyDuration = time.Second
	cfg.DownloadDuration = time.Second
	cfg.UploadDuration = time.Second
	cfg.BidirectionalDuration = time.Second

	_, err := Prepare(ctx, cfg)
	auth, ok := errors.AsType[*AuthRequiredError](err)
	if !ok {
		t.Fatalf("unapproved native preparation = %v, want authentication challenge", err)
	}
	pending, err := BeginAuthorization(cfg, auth.URL)
	if err != nil {
		t.Fatal(err)
	}
	browser := &http.Client{
		Timeout:       10 * time.Second,
		CheckRedirect: func(*http.Request, []*http.Request) error { return http.ErrUseLastResponse },
	}
	do := func(method, target string, form url.Values, cookies ...*http.Cookie) *http.Response {
		t.Helper()
		var body io.Reader
		if form != nil {
			body = strings.NewReader(form.Encode())
		}
		req, err := http.NewRequestWithContext(ctx, method, target, body)
		if err != nil {
			t.Fatal(err)
		}
		req.Header.Set("Origin", public)
		req.Header.Set("Sec-Fetch-Site", "same-origin")
		if form != nil {
			req.Header.Set("Content-Type", "application/x-www-form-urlencoded")
		}
		for _, cookie := range cookies {
			req.AddCookie(cookie)
		}
		response, err := browser.Do(req)
		if err != nil {
			t.Fatal(err)
		}
		_, _ = io.Copy(io.Discard, response.Body)
		response.Body.Close()
		return response
	}
	cookie := func(response *http.Response, name string) *http.Cookie {
		t.Helper()
		for _, value := range response.Cookies() {
			if value.Name == name {
				return value
			}
		}
		t.Fatalf("response omitted %s cookie", name)
		return nil
	}
	login := do("GET", public+"/login", nil)
	if login.StatusCode != http.StatusOK {
		t.Fatalf("login status = %d", login.StatusCode)
	}
	nonce := cookie(login, "__Host-gm_login")
	password := do("POST", public+"/auth/password", url.Values{
		"csrf": {nonce.Value}, "password": {"correct horse battery staple"},
	}, nonce)
	if password.StatusCode != http.StatusSeeOther {
		t.Fatalf("password status = %d", password.StatusCode)
	}
	session := cookie(password, "__Host-gm_session")
	csrf := cookie(password, "__Host-gm_csrf")
	approval := do("GET", pending.BrowserURL, nil, session)
	if approval.StatusCode != http.StatusOK {
		t.Fatalf("approval page status = %d", approval.StatusCode)
	}
	approvalURL, err := url.Parse(pending.BrowserURL)
	if err != nil {
		t.Fatal(err)
	}
	approved := do("POST", public+"/auth/cli/approve", url.Values{
		"csrf": {csrf.Value}, "challenge": {approvalURL.Query().Get("challenge")},
	}, session)
	if approved.StatusCode != http.StatusOK {
		t.Fatalf("approval status = %d", approved.StatusCode)
	}
	token, err := pending.Poll(ctx)
	if err != nil {
		t.Fatal(err)
	}
	cfg.AuthOrigin, err = CanonicalServerOrigin(public)
	if err != nil {
		t.Fatal(err)
	}
	cfg.AuthToken = token
	prepared, err := Prepare(ctx, cfg)
	if err != nil {
		t.Fatalf("approved native preparation: %v", err)
	}
	if prepared.ThroughputTarget.Transport != wire.TransportWebTransport || prepared.LatencyTarget == nil || prepared.LatencyTarget.Transport != wire.TransportWebTransport {
		t.Fatalf("native preparation did not select WebTransport: throughput=%s latency=%v", prepared.ThroughputTarget.Transport, prepared.LatencyTarget)
	}
	var resultCount, downloadBytes, uploadBytes int
	var terminal bool
	var events sync.Mutex
	err = RunPrepared(ctx, cfg, prepared, func(event Event) {
		events.Lock()
		defer events.Unlock()
		switch event.Kind {
		case EventResult:
			if event.Result == nil || event.Result.Unavailable || event.Result.Err != nil {
				t.Errorf("%s result unavailable: %v", event.Stage, event.Result)
				return
			}
			resultCount++
			switch event.Direction {
			case Down:
				downloadBytes += int(event.Result.TotalBytes)
			case Up:
				uploadBytes += int(event.Result.TotalBytes)
			}
		case EventDone:
			terminal = true
			if event.Err != nil {
				t.Errorf("native run ended with error: %v", event.Err)
			}
		}
	})
	events.Lock()
	defer events.Unlock()
	if err != nil || !terminal || resultCount < 4 || downloadBytes == 0 || uploadBytes == 0 {
		t.Fatalf("native run: err=%v terminal=%t results=%d down=%d up=%d", err, terminal, resultCount, downloadBytes, uploadBytes)
	}
	t.Logf("Native Go client approved against Rust: %d results with transfer in both directions", resultCount)
}
