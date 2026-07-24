package endpoint

import (
	"context"
	"errors"
	"io"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync/atomic"
	"testing"
	"time"

	"github.com/coder/websocket"
	"github.com/zR-JB/graphite-meter/go/internal/transport"
)

/* ---- test doubles ---- */

// echoEndpoint writes its id into the response (HTTP) or bus (WS), so a test
// mounting several endpoints can tell which one answered a given path.
type echoEndpoint struct{ id string }

func (e *echoEndpoint) ID() string                 { return e.id }
func (e *echoEndpoint) Capabilities() Capabilities { return Capabilities{} }
func (e *echoEndpoint) Handle(s transport.Session) error {
	if w, _, ok := s.HTTP(); ok {
		_, _ = w.Write([]byte(e.id)) // test double: a failed write shows up as a body mismatch
		return nil
	}
	if bus, ok := s.Bus(); ok {
		return bus.Send(e.id)
	}
	return nil
}

// countingEndpoint is a call-counting, error-injecting Endpoint stub for
// httpAdapter/wsAdapter tests.
type countingEndpoint struct {
	calls atomic.Int32
	err   error
}

func (e *countingEndpoint) ID() string                 { return "counting" }
func (e *countingEndpoint) Capabilities() Capabilities { return Capabilities{} }
func (e *countingEndpoint) Handle(s transport.Session) error {
	e.calls.Add(1)
	return e.err
}

// blockingEndpoint's Handle blocks on the session's context and reports back
// through unblocked the instant it observes cancellation.
type blockingEndpoint struct {
	unblocked chan struct{}
}

func (e *blockingEndpoint) ID() string                 { return "blocking" }
func (e *blockingEndpoint) Capabilities() Capabilities { return Capabilities{} }
func (e *blockingEndpoint) Handle(s transport.Session) error {
	<-s.Context().Done()
	close(e.unblocked)
	return nil
}

func wsURL(httpURL string) string { return "ws" + strings.TrimPrefix(httpURL, "http") }

/* ---- tests ---- */

// TestMountResolvesHTTPAndWSIndependently checks Mount wires both an HTTP
// endpoint and a WS endpoint from the same registry onto one mux, each
// resolving only at its own path.
func TestMountResolvesHTTPAndWSIndependently(t *testing.T) {
	reg := NewRegistry()
	reg.RegisterHTTP("/http-ep", &echoEndpoint{id: "http-reply"})
	reg.RegisterWS("/ws-ep", &echoEndpoint{id: "ws-reply"})
	mux := http.NewServeMux()
	reg.Mount(context.Background(), mux)

	srv := httptest.NewServer(mux)
	defer srv.Close()

	res, err := http.Get(srv.URL + "/http-ep")
	if err != nil {
		t.Fatalf("get: %v", err)
	}
	defer res.Body.Close()
	body, err := io.ReadAll(res.Body)
	if err != nil {
		t.Fatalf("read body: %v", err)
	}
	if got := string(body); got != "http-reply" {
		t.Errorf("/http-ep body = %q, want %q", got, "http-reply")
	}

	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	conn, _, err := websocket.Dial(ctx, wsURL(srv.URL)+"/ws-ep", nil)
	if err != nil {
		t.Fatalf("dial: %v", err)
	}
	defer conn.Close(websocket.StatusNormalClosure, "")
	_, data, err := conn.Read(ctx)
	if err != nil {
		t.Fatalf("read: %v", err)
	}
	if got := string(data); got != "ws-reply" {
		t.Errorf("/ws-ep message = %q, want %q", got, "ws-reply")
	}
}

// TestHTTPAdapterSetsCommonHeaders checks setCommonHeaders is applied on both
// a successful response and an error response.
func TestHTTPAdapterSetsCommonHeaders(t *testing.T) {
	check := func(t *testing.T, res *http.Response) {
		t.Helper()
		if got := res.Header.Get("Access-Control-Allow-Origin"); got != "*" {
			t.Errorf("Access-Control-Allow-Origin = %q, want *", got)
		}
		if got := res.Header.Get("Access-Control-Allow-Methods"); got != "GET, POST, DELETE, OPTIONS" {
			t.Errorf("Access-Control-Allow-Methods = %q", got)
		}
		if got := res.Header.Get("Access-Control-Allow-Headers"); got != "*" {
			t.Errorf("Access-Control-Allow-Headers = %q, want *", got)
		}
		if got := res.Header.Get("Timing-Allow-Origin"); got != "*" {
			t.Errorf("Timing-Allow-Origin = %q, want *", got)
		}
	}

	t.Run("success", func(t *testing.T) {
		srv := httptest.NewServer(httpAdapter(&countingEndpoint{}))
		defer srv.Close()
		res, err := http.Get(srv.URL)
		if err != nil {
			t.Fatalf("get: %v", err)
		}
		defer res.Body.Close()
		check(t, res)
	})

	t.Run("error", func(t *testing.T) {
		e := &countingEndpoint{err: errBoom}
		srv := httptest.NewServer(httpAdapter(e))
		defer srv.Close()
		res, err := http.Get(srv.URL)
		if err != nil {
			t.Fatalf("get: %v", err)
		}
		defer res.Body.Close()
		check(t, res)
		if res.StatusCode != http.StatusInternalServerError {
			t.Errorf("status = %d, want %d", res.StatusCode, http.StatusInternalServerError)
		}
	})
}

// TestHTTPAdapterOptionsShortCircuits checks a CORS preflight OPTIONS gets a
// bare 204 and never reaches the wrapped endpoint's Handle.
func TestHTTPAdapterOptionsShortCircuits(t *testing.T) {
	e := &countingEndpoint{}
	srv := httptest.NewServer(httpAdapter(e))
	defer srv.Close()

	req, err := http.NewRequest(http.MethodOptions, srv.URL, nil)
	if err != nil {
		t.Fatalf("new request: %v", err)
	}
	res, err := http.DefaultClient.Do(req)
	if err != nil {
		t.Fatalf("do: %v", err)
	}
	defer res.Body.Close()

	if res.StatusCode != http.StatusNoContent {
		t.Errorf("status = %d, want %d", res.StatusCode, http.StatusNoContent)
	}
	if n := e.calls.Load(); n != 0 {
		t.Errorf("Handle called %d times for OPTIONS, want 0", n)
	}
}

// TestHTTPAdapterHandleErrorReturns500 checks a Handle error surfaces as a 500
// with the error text in the body.
func TestHTTPAdapterHandleErrorReturns500(t *testing.T) {
	e := &countingEndpoint{err: errBoom}
	srv := httptest.NewServer(httpAdapter(e))
	defer srv.Close()

	res, err := http.Get(srv.URL)
	if err != nil {
		t.Fatalf("get: %v", err)
	}
	defer res.Body.Close()

	if res.StatusCode != http.StatusInternalServerError {
		t.Errorf("status = %d, want %d", res.StatusCode, http.StatusInternalServerError)
	}
	body, err := io.ReadAll(res.Body)
	if err != nil {
		t.Fatalf("read body: %v", err)
	}
	if got := string(body); !strings.Contains(got, errBoom.Error()) {
		t.Errorf("body = %q, want it to contain %q", got, errBoom.Error())
	}
}

// TestWSAdapterUpgradeSucceeds checks a plain upgrade with a Handle that
// returns nil closes normally.
func TestWSAdapterUpgradeSucceeds(t *testing.T) {
	e := &countingEndpoint{}
	mux := http.NewServeMux()
	mux.Handle("/ws", wsAdapter(context.Background(), e))
	srv := httptest.NewServer(mux)
	defer srv.Close()

	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	conn, _, err := websocket.Dial(ctx, wsURL(srv.URL)+"/ws", nil)
	if err != nil {
		t.Fatalf("dial: %v", err)
	}
	defer conn.Close(websocket.StatusNormalClosure, "")

	_, _, err = conn.Read(ctx)
	if websocket.CloseStatus(err) != websocket.StatusNormalClosure {
		t.Fatalf("close status = %v (err %v), want StatusNormalClosure", websocket.CloseStatus(err), err)
	}
	if n := e.calls.Load(); n != 1 {
		t.Errorf("Handle called %d times, want 1", n)
	}
}

// TestWSAdapterHandleErrorClosesWithInternalError checks a Handle error after
// a successful upgrade closes the connection with StatusInternalError.
func TestWSAdapterHandleErrorClosesWithInternalError(t *testing.T) {
	e := &countingEndpoint{err: errBoom}
	mux := http.NewServeMux()
	mux.Handle("/ws", wsAdapter(context.Background(), e))
	srv := httptest.NewServer(mux)
	defer srv.Close()

	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	conn, _, err := websocket.Dial(ctx, wsURL(srv.URL)+"/ws", nil)
	if err != nil {
		t.Fatalf("dial: %v", err)
	}
	defer conn.Close(websocket.StatusNormalClosure, "")

	_, _, err = conn.Read(ctx)
	if got := websocket.CloseStatus(err); got != websocket.StatusInternalError {
		t.Fatalf("close status = %v (err %v), want StatusInternalError", got, err)
	}
}

// TestHTTPAdapterOptionsIgnoresRequestHeaders checks the permissive-CORS design
// point: a preflight OPTIONS carrying Origin and both Access-Control-Request
// headers still gets the same wildcard response. Public mode is cookie-less, so
// nothing is reflected or validated per-request.
func TestHTTPAdapterOptionsIgnoresRequestHeaders(t *testing.T) {
	e := &countingEndpoint{}
	srv := httptest.NewServer(httpAdapter(e))
	defer srv.Close()

	req, err := http.NewRequest(http.MethodOptions, srv.URL, nil)
	if err != nil {
		t.Fatalf("new request: %v", err)
	}
	req.Header.Set("Origin", "https://evil.example")
	req.Header.Set("Access-Control-Request-Method", "PUT")
	req.Header.Set("Access-Control-Request-Headers", "X-Custom-Header, X-Another")

	res, err := http.DefaultClient.Do(req)
	if err != nil {
		t.Fatalf("do: %v", err)
	}
	defer res.Body.Close()

	if res.StatusCode != http.StatusNoContent {
		t.Errorf("status = %d, want %d", res.StatusCode, http.StatusNoContent)
	}
	if got := res.Header.Get("Access-Control-Allow-Origin"); got != "*" {
		t.Errorf("Access-Control-Allow-Origin = %q, want * regardless of Origin", got)
	}
	if got := res.Header.Get("Access-Control-Allow-Headers"); got != "*" {
		t.Errorf("Access-Control-Allow-Headers = %q, want * regardless of the requested headers", got)
	}
	if n := e.calls.Load(); n != 0 {
		t.Errorf("Handle called %d times for a preflight OPTIONS, want 0", n)
	}
}

// TestMountLongestPathWins checks a subtree ("/api/") and a more specific
// literal ("/api/specific") on one registry resolve by ServeMux's longest-match
// rule whatever the map iteration order. Mount is a thin, order-independent
// wrapper and must not break that precedence when paths overlap.
func TestMountLongestPathWins(t *testing.T) {
	reg := NewRegistry()
	reg.RegisterHTTP("/api/", &echoEndpoint{id: "subtree"})
	reg.RegisterHTTP("/api/specific", &echoEndpoint{id: "specific"})
	mux := http.NewServeMux()
	reg.Mount(context.Background(), mux)

	srv := httptest.NewServer(mux)
	defer srv.Close()

	check := func(path, want string) {
		t.Helper()
		res, err := http.Get(srv.URL + path)
		if err != nil {
			t.Fatalf("get %s: %v", path, err)
		}
		defer res.Body.Close()
		body, err := io.ReadAll(res.Body)
		if err != nil {
			t.Fatalf("read body for %s: %v", path, err)
		}
		if got := string(body); got != want {
			t.Errorf("%s body = %q, want %q", path, got, want)
		}
	}
	check("/api/specific", "specific")
	check("/api/other", "subtree")
}

// TestMountShutdownCancelUnblocksWSHandler checks the context Mount is given
// bounds every bus handler's lifetime: cancelling it while a handler is parked
// on ctx.Done() (as conn.Read/Write would be) unblocks that handler promptly.
func TestMountShutdownCancelUnblocksWSHandler(t *testing.T) {
	e := &blockingEndpoint{unblocked: make(chan struct{})}
	parent, cancelParent := context.WithCancel(context.Background())
	defer cancelParent()

	reg := NewRegistry()
	reg.RegisterWS("/ws", e)
	mux := http.NewServeMux()
	reg.Mount(parent, mux)
	srv := httptest.NewServer(mux)
	defer srv.Close()

	dialCtx, cancelDial := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancelDial()
	conn, _, err := websocket.Dial(dialCtx, wsURL(srv.URL)+"/ws", nil)
	if err != nil {
		t.Fatalf("dial: %v", err)
	}
	defer conn.Close(websocket.StatusNormalClosure, "")

	cancelParent()

	select {
	case <-e.unblocked:
	case <-time.After(2 * time.Second):
		t.Fatal("Handle did not unblock after the parent context was cancelled")
	}
}

var errBoom = errors.New("boom")
