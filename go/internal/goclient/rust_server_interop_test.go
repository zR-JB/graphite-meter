package goclient

import (
	"context"
	"errors"
	"io"
	"net/http"
	"net/url"
	"os"
	"strings"
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

	controller := NewController(ctx)
	defer controller.Close()
	preparation := controller.NewPreparation(cfg, nil)
	prepared, err := preparation.PrepareRun()
	auth, ok := errors.AsType[*AuthRequiredError](err)
	if !ok {
		t.Fatalf("unapproved native preparation = %v, want authentication challenge", err)
	}
	origin, err := wire.CanonicalOrigin(public)
	if err != nil {
		t.Fatal(err)
	}
	pending, err := preparation.BeginAuthorization(origin, auth.URL)
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
	token, err := preparation.PollAuthorization(pending)
	if err != nil {
		t.Fatal(err)
	}
	if err := controller.AcceptAuthorization(pending.Origin, token); err != nil {
		t.Fatal(err)
	}
	prepared, err = controller.NewPreparation(cfg, prepared).PrepareRun()
	if err != nil || !prepared.Ready() {
		t.Fatalf("approved native preparation: %v", err)
	}
	connection := prepared.Servers[0].Connection
	if connection.ThroughputTarget.Transport != wire.TransportWebTransport || connection.LatencyTarget == nil ||
		connection.LatencyTarget.Transport != wire.TransportWebTransport {
		t.Fatalf("native preparation did not select WebTransport: throughput=%s latency=%v",
			connection.ThroughputTarget.Transport, connection.LatencyTarget)
	}
	var resultCount, downloadBytes, uploadBytes int
	var done *Event
	for event := range controller.Start(cfg, prepared) {
		switch event.Kind {
		case EventResult:
			if event.Result.Unavailable || event.Result.Err != nil {
				t.Errorf("%s result unavailable: %+v", event.Stage, event.Result)
				continue
			}
			resultCount++
			switch event.Direction {
			case Down:
				downloadBytes += int(event.Result.TotalBytes)
			case Up:
				uploadBytes += int(event.Result.TotalBytes)
			}
		case EventServerFailure:
			t.Errorf("%s left the %s stage: %v", event.ServerID, event.Stage, event.Failure.Err)
		case EventDone:
			done = &event
		}
	}
	if done == nil || done.Err != nil || done.Outcome() != OutcomeComplete || resultCount < 4 ||
		downloadBytes == 0 || uploadBytes == 0 {
		t.Fatalf("native run: done=%+v results=%d down=%d up=%d", done, resultCount, downloadBytes, uploadBytes)
	}
	t.Logf("Native Go client approved against Rust: %d results with transfer in both directions", resultCount)
}
