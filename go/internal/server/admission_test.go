package server

import (
	"context"
	"fmt"
	"io"
	"net/http"
	"net/http/httptest"
	"net/netip"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"testing/synctest"
	"time"

	"github.com/coder/websocket"
	"github.com/zR-JB/graphite-meter/go/internal/config"
	"github.com/zR-JB/graphite-meter/go/internal/endpoint"
	"github.com/zR-JB/graphite-meter/go/internal/route"
)

func routeSpec(path string) route.Spec {
	spec, _ := route.Lookup(path)
	return spec
}

func TestRequestAdmissionPerClientAndRelease(t *testing.T) {
	synctest.Test(t, requestAdmissionPerClientAndRelease)
}

func requestAdmissionPerClientAndRelease(t *testing.T) {
	a := newRequestAdmission(3, 2, 3, 4, time.Minute, time.Hour)
	entered := make(chan struct{}, 3)
	release := make(chan struct{})
	h := a.wrap(http.HandlerFunc(func(http.ResponseWriter, *http.Request) {
		entered <- struct{}{}
		<-release
	}), routeSpec(route.Download), nil, publicAuth(t))

	var wg sync.WaitGroup
	for range 2 {
		wg.Go(func() {
			r := httptest.NewRequest(http.MethodGet, "/download", nil)
			r.RemoteAddr = "192.0.2.10:1234"
			h.ServeHTTP(httptest.NewRecorder(), r)
		})
	}
	synctest.Wait()
	if len(entered) != 2 {
		t.Fatalf("%d of 2 requests within the per-client budget entered", len(entered))
	}
	r := httptest.NewRequest(http.MethodGet, "/download", nil)
	r.RemoteAddr = "192.0.2.10:5678"
	w := httptest.NewRecorder()
	h.ServeHTTP(w, r)
	if w.Code != http.StatusTooManyRequests || w.Header().Get("Retry-After") != "1" ||
		w.Header().Get("Access-Control-Allow-Origin") != "*" {
		t.Fatalf("rejection = %d headers %v, want 429 with Retry-After 1 and a wildcard origin", w.Code, w.Header())
	}
	close(release)
	wg.Wait()
	w = httptest.NewRecorder()
	h.ServeHTTP(w, httptest.NewRequest(http.MethodGet, "/download", nil))
	if w.Code != http.StatusOK {
		t.Fatalf("request after release = %d, want %d", w.Code, http.StatusOK)
	}
}

func TestUploadAdmissionReleasesStalledBody(t *testing.T) {
	t.Parallel()
	for _, http2 := range []bool{false, true} {
		name := "http1"
		if http2 {
			name = "http2"
		}
		t.Run(name, func(t *testing.T) {
			t.Parallel()
			a := newRequestAdmission(1, 1, 1, 4, 50*time.Millisecond, time.Hour)
			upload := endpoint.NewUpload(nil, nil)
			id := upload.Mint()
			finished := make(chan struct{})
			h := a.wrap(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				if (r.ProtoMajor == 2) != http2 {
					t.Errorf("request protocol = %s, HTTP/2 enabled = %t", r.Proto, http2)
				}
				upload.ServeHTTP(w, r)
			}), routeSpec(route.Upload), nil, publicAuth(t))
			srv := httptest.NewUnstartedServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				h.ServeHTTP(w, r)
				close(finished)
			}))
			srv.EnableHTTP2 = http2
			srv.StartTLS()
			defer srv.Close()
			body, writer := io.Pipe()
			defer body.Close()
			defer writer.Close()
			ctx, cancel := context.WithTimeout(t.Context(), 5*time.Second)
			defer cancel()
			req, err := http.NewRequestWithContext(ctx, http.MethodPost, srv.URL+"/upload?id="+id, body)
			if err != nil {
				t.Fatal(err)
			}
			clientDone := make(chan struct{})
			go func() {
				defer close(clientDone)
				res, err := srv.Client().Do(req)
				if err == nil {
					res.Body.Close()
				}
			}()
			defer func() {
				cancel()
				writer.Close()
				<-clientDone
			}()
			select {
			case <-finished:
				if requests, _ := a.stats(); requests.active != 0 {
					t.Fatalf("active uploads = %d after timeout, want 0", requests.active)
				}
			case <-ctx.Done():
				t.Fatal("stalled upload retained its admission slot beyond the request lifetime")
			}
		})
	}
}

// Requests spend the pool and a per-client share; sessions spend the pool and a per-login share of a session budget
// that caps them without reserving anything.
func TestRequestAdmissionBudgets(t *testing.T) {
	type step struct {
		key, login string
		want       int
	}
	for _, tc := range []struct {
		name                          string
		pool, client, sessions, login int
		steps                         []step
		poolRefusals, sessionRefusals uint64
	}{
		{"pool", 1, 1, 1, 4, []step{{"a", "", 0}, {"b", "", 503}}, 1, 0},
		{"sessions per login", 100, 100, 100, 2,
			[]step{{"c", "s", 0}, {"c", "s", 0}, {"c", "s", 429}, {"c", "", 0}}, 0, 0},
		{"logins of one client", 100, 1, 100, 1,
			[]step{{"c", "phone", 0}, {"c", "desktop", 0}, {"c", "phone", 429}, {"c", "", 0}}, 0, 0},
		{"session budget", 10, 10, 2, 10,
			[]step{{"a", "la", 0}, {"b", "lb", 0}, {"c", "lc", 503}, {"c", "", 0}, {"d", "", 0}}, 0, 1},
		{"ceiling, not reservation", 4, 2, 4, 2,
			[]step{{"a", "", 0}, {"a", "", 0}, {"b", "", 0}, {"b", "", 0}, {"c", "lc", 503}}, 1, 0},
		{"separate refusal counters", 4, 4, 1, 4, []step{{"a", "la", 0}, {"b", "lb", 503},
			{"c", "", 0}, {"c", "", 0}, {"c", "", 0}, {"d", "", 503}}, 1, 1},
		{"IPv6 aggregates double", 100, 1, 100, 1, []step{{"a b x", "", 0}, {"c b x", "", 0}, {"d b x", "", 429},
			{"e f x", "", 0}, {"g f x", "", 0}, {"h i x", "", 429}}, 0, 0},
	} {
		t.Run(tc.name, func(t *testing.T) {
			a := newRequestAdmission(tc.pool, tc.client, tc.sessions, tc.login, time.Minute, time.Hour)
			for i, step := range tc.steps {
				keys := strings.Fields(step.key)
				if step.login != "" {
					keys = []string{step.login}
				}
				if release, status := a.acquire(step.login != "", keys...); status != step.want {
					t.Fatalf("step %d %+v = %d", i, step, status)
				} else if status == 0 {
					defer release()
				}
			}
			if requests, sessions := a.stats(); requests.rejectedGlobal != tc.poolRefusals ||
				sessions.rejectedGlobal != tc.sessionRefusals {
				t.Fatalf("refusals = %d pool / %d session", requests.rejectedGlobal, sessions.rejectedGlobal)
			}
		})
	}
}

// A request-shaped route and both ping buses take the request bound and budget; only the transfer sessions hold a test.
func TestAdmissionLifetimeFollowsTheRouteBudget(t *testing.T) {
	a := newRequestAdmission(100, 100, 100, 100, time.Minute, time.Hour)
	for path, session := range map[string]bool{
		route.Download: false, route.Ping: false, route.WTPing: false, route.WTDownload: true, route.WTUpload: true,
	} {
		var lifetime time.Duration
		var heldSession bool
		a.wrap(http.HandlerFunc(func(_ http.ResponseWriter, r *http.Request) {
			deadline, _ := r.Context().Deadline()
			lifetime = time.Until(deadline)
			_, sessions := a.stats()
			heldSession = sessions.active == 1
		}), routeSpec(path), nil, publicAuth(t)).ServeHTTP(httptest.NewRecorder(),
			httptest.NewRequest(http.MethodGet, path, nil))
		want := time.Minute
		if session {
			want = time.Hour
		}
		if lifetime <= want-time.Second || lifetime > want || heldSession != session {
			t.Errorf("%s lifetime = %v holding a session = %v, want %v and %v", path, lifetime, heldSession, want,
				session)
		}
	}
}

// deadlineRecordingWriter counts the socket deadlines wrap arms through http.NewResponseController.
type deadlineRecordingWriter struct {
	*httptest.ResponseRecorder
	read, write time.Time
}

func (w *deadlineRecordingWriter) SetReadDeadline(t time.Time) error  { w.read = t; return nil }
func (w *deadlineRecordingWriter) SetWriteDeadline(t time.Time) error { w.write = t; return nil }

// A socket deadline bounds a request; it would tear a held channel down mid-stream, so a channel clears the control
// deadline every request starts with.
func TestAdmissionSetsSocketDeadlinesByRouteKind(t *testing.T) {
	a := newRequestAdmission(100, 100, 100, 100, time.Minute, time.Hour)
	for path, bounded := range map[string]bool{
		route.Ping: false, route.WTPing: false, route.WTDownload: false, route.WTUpload: false,
		route.Download: true, route.Upload: true,
	} {
		w := &deadlineRecordingWriter{ResponseRecorder: httptest.NewRecorder(), read: time.Now(), write: time.Now()}
		a.wrap(http.HandlerFunc(func(http.ResponseWriter, *http.Request) {}), routeSpec(path), nil, publicAuth(t)).
			ServeHTTP(w, httptest.NewRequest(http.MethodGet, path, nil))
		if w.read.IsZero() == bounded || w.write.IsZero() == bounded {
			t.Errorf("%s socket deadlines = %v / %v, want bounded = %t", path, w.read, w.write, bounded)
		}
	}
}

func TestRequestAdmissionRejectsWebSocketBeforeUpgrade(t *testing.T) {
	a := newRequestAdmission(1, 1, 1, 4, time.Minute, time.Hour)
	release, status := a.acquire(false, "occupied")
	if status != 0 {
		t.Fatal("failed to occupy admission slot")
	}
	defer release()
	var reached atomic.Bool
	srv := httptest.NewServer(a.wrap(http.HandlerFunc(func(http.ResponseWriter, *http.Request) {
		reached.Store(true)
	}), routeSpec(route.Ping), nil, publicAuth(t)))
	defer srv.Close()
	ctx, cancel := context.WithTimeout(t.Context(), time.Second)
	defer cancel()
	_, res, err := websocket.Dial(ctx, "ws"+strings.TrimPrefix(srv.URL, "http"), nil)
	if err == nil {
		t.Fatal("saturated WebSocket upgrade succeeded")
	}
	if res == nil || res.StatusCode != http.StatusServiceUnavailable || res.Header.Get("Retry-After") != "1" {
		t.Fatalf("upgrade response = %#v, want 503 with Retry-After 1", res)
	}
	if reached.Load() {
		t.Fatal("rejected WebSocket reached handler")
	}
}

// A panic unwinds through wrap, so the slot returns before net/http recovers the panic.
func TestAdmissionReleasesTheSlotOfAPanickingHandler(t *testing.T) {
	a := newRequestAdmission(1, 1, 1, 1, time.Minute, time.Hour)
	for _, path := range []string{route.Download, route.WTDownload} {
		h := a.wrap(http.HandlerFunc(func(http.ResponseWriter, *http.Request) { panic(http.ErrAbortHandler) }),
			routeSpec(path), nil, publicAuth(t))
		func() {
			defer func() { _ = recover() }()
			h.ServeHTTP(httptest.NewRecorder(), httptest.NewRequest(http.MethodGet, path, nil))
		}()
		if requests, sessions := a.stats(); requests.active != 0 || sessions.active != 0 {
			t.Fatalf("%s panic left %d requests and %d sessions admitted", path, requests.active, sessions.active)
		}
	}
}

// Behind a trusted proxy an IPv6 client spends its /64, /56 and /48 shares; ambiguous evidence is refused.
func TestIPv6ClientsShareTheirAllocationsBudgets(t *testing.T) {
	cfg := config.Default()
	cfg.TrustedProxies = []netip.Prefix{netip.MustParsePrefix("127.0.0.0/8")}
	e := buildEndpoints(t.Context(), &cfg)
	srv := httptest.NewServer(newMux(t.Context(), e, muxTopology{transfers: true}, nil, publicAuth(t)))
	defer srv.Close()
	upload := func(realIP ...string) *http.Response {
		req, _ := http.NewRequest(http.MethodPost, srv.URL+"/upload?id="+e.upload.Mint(), strings.NewReader("x"))
		for _, ip := range realIP {
			req.Header.Add("X-Real-IP", ip)
		}
		res, err := srv.Client().Do(req)
		if err != nil {
			t.Fatal(err)
		}
		_, _ = io.Copy(io.Discard, res.Body)
		res.Body.Close()
		return res
	}
	for i := range 128 {
		subnet := i / 32
		if res := upload(fmt.Sprintf("2001:db8:0:%x::1", subnet/2<<8|subnet%2)); res.StatusCode != http.StatusOK {
			t.Fatalf("upload %d = %d", i, res.StatusCode)
		}
	}
	if res := upload("2001:db8:0:200::1"); res.StatusCode != http.StatusTooManyRequests ||
		res.Header.Get("X-Graphite-Upload-Refusal") != "clientFull" {
		t.Fatalf("a fresh /56 of a full /48 = %d %v", res.StatusCode, res.Header)
	}
	if res := upload("2001:db8:1::1"); res.StatusCode != http.StatusOK {
		t.Fatalf("another /48 = %d", res.StatusCode)
	}
	if res := upload("2001:db8:1::1", "2001:db8:2::1"); res.StatusCode != http.StatusBadRequest {
		t.Fatalf("ambiguous evidence = %d, want 400", res.StatusCode)
	}
}
