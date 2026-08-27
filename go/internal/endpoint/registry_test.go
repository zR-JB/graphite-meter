package endpoint

import (
	"context"
	"errors"
	"io"
	"net/http"
	"net/http/httptest"
	"slices"
	"strings"
	"sync/atomic"
	"testing"
	"time"

	"github.com/coder/websocket"
	"github.com/zR-JB/graphite-meter/go/internal/transport"
)

func wsAdapter(parent context.Context, e Endpoint) http.Handler {
	return wsAdapterWithOrigin(parent, e, "")
}
func httpAdapter(e Endpoint) http.Handler { return httpAdapterWithOrigin(e, "") }

/* ---- test doubles ---- */

// echoEndpoint writes its id into the response (HTTP) or bus (WS).
type echoEndpoint struct{ id string }

func (e *echoEndpoint) ID() string { return e.id }
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

// countingEndpoint is a call-counting, error-injecting Endpoint stub for httpAdapter/wsAdapter tests.
type countingEndpoint struct {
	calls atomic.Int32
	err   error
}

func (e *countingEndpoint) ID() string { return "counting" }
func (e *countingEndpoint) Handle(s transport.Session) error {
	e.calls.Add(1)
	return e.err
}

// blockingEndpoint's Handle blocks on the session's context and reports back through unblocked the instant it observes.
type blockingEndpoint struct {
	unblocked chan struct{}
}

func (e *blockingEndpoint) ID() string { return "blocking" }
func (e *blockingEndpoint) Handle(s transport.Session) error {
	<-s.Context().Done()
	close(e.unblocked)
	return nil
}

// drainEndpoint reads the bus until it errors, the shape every bus endpoint has.
type drainEndpoint struct{}

func (e *drainEndpoint) ID() string { return "drain" }
func (e *drainEndpoint) Handle(s transport.Session) error {
	bus, ok := s.Bus()
	if !ok {
		return transport.ErrUnsupported
	}
	for {
		if _, err := bus.Recv(); err != nil {
			return nil
		}
	}
}

func wsURL(httpURL string) string { return "ws" + strings.TrimPrefix(httpURL, "http") }

/* ---- tests ---- */

func TestMountResolvesHTTPAndWSIndependently(t *testing.T) {
	reg := NewRegistry()
	reg.RegisterHTTP("/http-ep", &echoEndpoint{id: "http-reply"})
	reg.RegisterWS("/ws-ep", &echoEndpoint{id: "ws-reply"})
	mux := http.NewServeMux()
	reg.Mount(t.Context(), mux)

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

	ctx, cancel := context.WithTimeout(t.Context(), 5*time.Second)
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

func TestWSAdapterUpgradeSucceeds(t *testing.T) {
	e := &countingEndpoint{}
	mux := http.NewServeMux()
	mux.Handle("/ws", wsAdapter(t.Context(), e))
	srv := httptest.NewServer(mux)
	defer srv.Close()

	ctx, cancel := context.WithTimeout(t.Context(), 5*time.Second)
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

func TestWSAdapterHandleErrorClosesWithInternalError(t *testing.T) {
	e := &countingEndpoint{err: errBoom}
	mux := http.NewServeMux()
	mux.Handle("/ws", wsAdapter(t.Context(), e))
	srv := httptest.NewServer(mux)
	defer srv.Close()

	ctx, cancel := context.WithTimeout(t.Context(), 5*time.Second)
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

// A named origin means the server holds session state, so the wildcard public mode answers with would be a real hole.
func TestHTTPAdapterNarrowsCORSToANamedOrigin(t *testing.T) {
	const origin = "https://ui.example"
	srv := httptest.NewServer(httpAdapterWithOrigin(&countingEndpoint{}, origin))
	defer srv.Close()

	res, err := http.Get(srv.URL)
	if err != nil {
		t.Fatalf("get: %v", err)
	}
	defer res.Body.Close()

	if got := res.Header.Get("Access-Control-Allow-Origin"); got != origin {
		t.Errorf("Access-Control-Allow-Origin = %q, want %q: an authenticated origin must never answer with a wildcard", got, origin)
	}
	if got := res.Header.Get("Access-Control-Allow-Credentials"); got != "true" {
		t.Errorf("Access-Control-Allow-Credentials = %q, want %q: the UI's credentialed requests would be rejected by the browser", got, "true")
	}
	if got := res.Header.Get("Timing-Allow-Origin"); got != origin {
		t.Errorf("Timing-Allow-Origin = %q, want %q", got, origin)
	}
	if got := res.Header.Get("Access-Control-Allow-Headers"); got == "*" {
		t.Error("Access-Control-Allow-Headers = *, want the named set: a wildcard is ignored for credentialed requests")
	}
	if got := res.Header.Values("Vary"); !slices.Contains(got, "Origin") {
		t.Errorf("Vary = %v, want it to contain Origin: a cache would serve one origin's response to another", got)
	}
}

// An oversized frame is a peer forcing the server to buffer.
func TestWSAdapterBoundsFrameSize(t *testing.T) {
	mux := http.NewServeMux()
	mux.Handle("/ws", wsAdapter(t.Context(), &drainEndpoint{}))
	srv := httptest.NewServer(mux)
	defer srv.Close()

	ctx, cancel := context.WithTimeout(t.Context(), 5*time.Second)
	defer cancel()
	conn, _, err := websocket.Dial(ctx, wsURL(srv.URL)+"/ws", nil)
	if err != nil {
		t.Fatalf("dial: %v", err)
	}
	defer conn.Close(websocket.StatusNormalClosure, "")

	if err := conn.Write(ctx, websocket.MessageText, make([]byte, 4096)); err != nil {
		t.Fatalf("write: %v", err)
	}
	readCtx, cancelRead := context.WithTimeout(t.Context(), 3*time.Second)
	defer cancelRead()
	_, _, err = conn.Read(readCtx)
	if got := websocket.CloseStatus(err); got != websocket.StatusMessageTooBig {
		t.Fatalf("close status = %v (err %v), want StatusMessageTooBig: an oversized frame was buffered instead of refused", got, err)
	}
}

func TestMountLongestPathWins(t *testing.T) {
	reg := NewRegistry()
	reg.RegisterHTTP("/api/", &echoEndpoint{id: "subtree"})
	reg.RegisterHTTP("/api/specific", &echoEndpoint{id: "specific"})
	mux := http.NewServeMux()
	reg.Mount(t.Context(), mux)

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

func TestMountShutdownCancelUnblocksWSHandler(t *testing.T) {
	e := &blockingEndpoint{unblocked: make(chan struct{})}
	parent, cancelParent := context.WithCancel(t.Context())
	defer cancelParent()

	reg := NewRegistry()
	reg.RegisterWS("/ws", e)
	mux := http.NewServeMux()
	reg.Mount(parent, mux)
	srv := httptest.NewServer(mux)
	defer srv.Close()

	dialCtx, cancelDial := context.WithTimeout(t.Context(), 5*time.Second)
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
