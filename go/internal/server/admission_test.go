package server

import (
	"fmt"
	"io"
	"net/http"
	"net/http/httptest"
	"net/netip"
	"strings"
	"testing"
	"time"

	"github.com/zR-JB/graphite-meter/go/internal/config"
	"github.com/zR-JB/graphite-meter/go/internal/route"
)

func routeSpec(path string) route.Spec {
	spec, _ := route.Lookup(path)
	return spec
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

// deadlineRecordingWriter records the socket deadlines wrap arms through http.NewResponseController.
type deadlineRecordingWriter struct {
	*httptest.ResponseRecorder
	read, write time.Time
}

func (w *deadlineRecordingWriter) SetReadDeadline(t time.Time) error  { w.read = t; return nil }
func (w *deadlineRecordingWriter) SetWriteDeadline(t time.Time) error { w.write = t; return nil }

// Only the transfer sessions hold a session and its bound; a socket deadline would tear a held channel down mid-stream.
func TestAdmissionFollowsTheRouteKind(t *testing.T) {
	a := newRequestAdmission(100, 100, 100, 100, time.Minute, time.Hour)
	for path, want := range map[string]struct{ session, socketBounded bool }{
		route.Download: {false, true}, route.Upload: {false, true}, route.Ping: {false, false},
		route.WTPing: {false, false}, route.WTDownload: {true, false}, route.WTUpload: {true, false},
	} {
		var lifetime time.Duration
		var heldSession bool
		w := &deadlineRecordingWriter{ResponseRecorder: httptest.NewRecorder(), read: time.Now(), write: time.Now()}
		a.wrap(http.HandlerFunc(func(_ http.ResponseWriter, r *http.Request) {
			deadline, _ := r.Context().Deadline()
			lifetime = time.Until(deadline)
			_, sessions := a.stats()
			heldSession = sessions.active == 1
		}), routeSpec(path), nil, publicAuth(t)).ServeHTTP(w, httptest.NewRequest(http.MethodGet, path, nil))
		wantLifetime := time.Minute
		if want.session {
			wantLifetime = time.Hour
		}
		if lifetime <= wantLifetime-time.Second || lifetime > wantLifetime || heldSession != want.session ||
			w.read.IsZero() == want.socketBounded || w.write.IsZero() == want.socketBounded {
			t.Errorf("%s lifetime %v, session %t, socket deadlines %v / %v; want %v and %+v", path, lifetime,
				heldSession, w.read, w.write, wantLifetime, want)
		}
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
