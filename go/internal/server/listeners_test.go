package server

import (
	"bytes"
	"context"
	"crypto/tls"
	"errors"
	"io"
	"net"
	"net/http"
	"net/http/httptest"
	"strconv"
	"strings"
	"testing"
	"time"

	"github.com/quic-go/webtransport-go"
	"github.com/zR-JB/graphite-meter/go/internal/auth"
	"github.com/zR-JB/graphite-meter/go/internal/config"
	"github.com/zR-JB/graphite-meter/go/internal/static"
	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

type observedBody struct {
	reader *bytes.Reader
	read   int
}

func (b *observedBody) Read(p []byte) (int, error) {
	n, err := b.reader.Read(p)
	b.read += n
	return n, err
}

func testEndpoints(t *testing.T) *endpoints {
	t.Helper()
	cfg := config.Default()
	e, err := buildEndpoints(t.Context(), &cfg)
	if err != nil {
		t.Fatal(err)
	}
	return e
}

func TestH3BootstrapCannotServeTransfers(t *testing.T) {
	e := testEndpoints(t)
	mux := listenerMuxConfigured(t.Context(), e, muxTopology{bootstrap: true}, static.Handler(), nil)
	for _, path := range []string{"/download", "/upload", "/upload/session", "/upload/progress", "/ws/ping"} {
		rec := httptest.NewRecorder()
		mux.ServeHTTP(rec, httptest.NewRequest(http.MethodGet, path, nil))
		if rec.Code != http.StatusNotFound {
			t.Errorf("%s status = %d, want 404", path, rec.Code)
		}
	}
}

func TestH2ThroughputRoutesRequireHTTP2(t *testing.T) {
	e := testEndpoints(t)
	mux := listenerMuxConfigured(t.Context(), e, muxTopology{transfers: true, requiredProto: 2}, static.Handler(), nil)
	rec := httptest.NewRecorder()
	req := httptest.NewRequest(http.MethodGet, "/download?bytes=1", nil)
	mux.ServeHTTP(rec, req)
	if rec.Code != http.StatusNotFound {
		t.Fatalf("h1 transfer status = %d, want 404", rec.Code)
	}
	rec = httptest.NewRecorder()
	mux.ServeHTTP(rec, httptest.NewRequest(http.MethodGet, "/ws/ping", nil))
	if rec.Code != http.StatusNotFound {
		t.Fatalf("H2 websocket status = %d, want 404", rec.Code)
	}
}

func TestH2MountsOnlyMeasurementHTTPRoutes(t *testing.T) {
	e := testEndpoints(t)
	mux := listenerMuxConfigured(t.Context(), e, muxTopology{transfers: true, requiredProto: 2}, static.Handler(), nil)
	for _, path := range []string{"/", "/assets/app.js", "/preflight", "/ws/ping"} {
		rec := httptest.NewRecorder()
		req := httptest.NewRequest(http.MethodGet, path, nil)
		req.Proto, req.ProtoMajor, req.ProtoMinor = "HTTP/2.0", 2, 0
		mux.ServeHTTP(rec, req)
		if rec.Code != http.StatusNotFound {
			t.Errorf("%s status = %d, want 404", path, rec.Code)
		}
	}
	for _, path := range []string{"/probe", "/download?bytes=1", "/upload/session", "/upload", "/upload/progress"} {
		rec := httptest.NewRecorder()
		req := httptest.NewRequest(http.MethodGet, path, nil)
		req.Proto, req.ProtoMajor, req.ProtoMinor = "HTTP/2.0", 2, 0
		mux.ServeHTTP(rec, req)
		if rec.Code == http.StatusNotFound {
			t.Errorf("%s is not mounted", path)
		}
	}
}

func TestH1MountsSPAAndDiscovery(t *testing.T) {
	e := testEndpoints(t)
	mux := listenerMuxConfigured(t.Context(), e, muxTopology{spa: true, discovery: true, latency: true, transfers: true}, http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		w.WriteHeader(http.StatusOK)
	}), nil)
	for _, path := range []string{"/", "/preflight"} {
		rec := httptest.NewRecorder()
		mux.ServeHTTP(rec, httptest.NewRequest(http.MethodGet, path, nil))
		if rec.Code != http.StatusOK {
			t.Errorf("%s status = %d, want 200", path, rec.Code)
		}
	}
}

func TestH1RejectsDotSegmentsBeforeServeMuxCanonicalization(t *testing.T) {
	e := testEndpoints(t)
	mux := listenerMuxConfigured(t.Context(), e, muxTopology{spa: true}, http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		_, _ = w.Write([]byte("shell"))
	}), nil)
	for _, path := range []string{
		"/foo/..",
		"/assets/..",
		"/foo/%2e%2e",
		`/foo\..\bar`,
	} {
		recorder := httptest.NewRecorder()
		mux.ServeHTTP(recorder, httptest.NewRequest(http.MethodGet, path, nil))
		if recorder.Code != http.StatusNotFound {
			t.Errorf("%s status = %d, want 404", path, recorder.Code)
		}
		if strings.Contains(recorder.Body.String(), "shell") {
			t.Errorf("%s reached the SPA handler", path)
		}
	}
}

func TestAuthenticationWrapsEveryFinalListenerBeforeDispatch(t *testing.T) {
	authn := testPasswordAuth(t, "https://meter.example")
	e := testEndpoints(t)
	tests := []struct {
		name     string
		topology muxTopology
		listener auth.Listener
		path     string
		proto    int
	}{
		{"h1-ui", muxTopology{spa: true, discovery: true, latency: true, transfers: true}, auth.Listener{UI: true}, "/", 1},
		{"h1-static", muxTopology{spa: true, discovery: true, latency: true, transfers: true}, auth.Listener{UI: true}, "/asset.js", 1},
		{"h1-upload", muxTopology{spa: true, discovery: true, latency: true, transfers: true}, auth.Listener{UI: true}, "/upload", 1},
		{"h2", muxTopology{transfers: true, requiredProto: 2}, auth.Listener{}, "/download", 2},
		{"h3-bootstrap", muxTopology{bootstrap: true}, auth.Listener{}, "/probe", 1},
		{"h3", muxTopology{transfers: true}, auth.Listener{}, "/upload", 3},
		{"websocket", muxTopology{latency: true}, auth.Listener{}, "/ws/ping", 1},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			body := &observedBody{reader: bytes.NewReader(bytes.Repeat([]byte("x"), 1024))}
			mux := listenerMuxConfigured(t.Context(), e, test.topology, http.HandlerFunc(func(http.ResponseWriter, *http.Request) { t.Fatal("SPA dispatched") }), authn)
			handler := authn.Enforce(mux, test.listener)
			req := httptest.NewRequest(http.MethodPost, "https://meter.example"+test.path, body)
			req.Host = "meter.example"
			req.TLS = &tls.ConnectionState{}
			req.ProtoMajor = test.proto
			recorder := httptest.NewRecorder()
			handler.ServeHTTP(recorder, req)
			if recorder.Code != http.StatusForbidden || body.read != 0 {
				t.Fatalf("status=%d body-read=%d, want 403 with an unread body", recorder.Code, body.read)
			}
		})
	}
	if stats := e.admission.stats(); stats.active != 0 || stats.peak != 0 {
		t.Fatalf("unauthenticated requests reached admission: %+v", stats)
	}
}

func TestH1MountsLatencyAndH3MountsProgress(t *testing.T) {
	e := testEndpoints(t)
	h1 := listenerMuxConfigured(t.Context(), e, muxTopology{discovery: true, latency: true, transfers: true, requiredProto: 1}, static.Handler(), nil)
	rec := httptest.NewRecorder()
	h1.ServeHTTP(rec, httptest.NewRequest(http.MethodGet, "/ws/ping", nil))
	if rec.Code == http.StatusNotFound {
		t.Fatal("H1 latency websocket is not mounted")
	}
	h3 := listenerMuxConfigured(t.Context(), e, muxTopology{transfers: true}, static.Handler(), nil)
	rec = httptest.NewRecorder()
	h3.ServeHTTP(rec, httptest.NewRequest(http.MethodGet, "/upload/progress?id=unknown", nil))
	if rec.Code == http.StatusNotFound {
		t.Fatal("H3 upload progress is not mounted")
	}
}

func TestPublicH3Port(t *testing.T) {
	cfg := config.Default()
	cfg.Native.H3 = ":7249"
	if got := publicH3Port(&cfg); got != "7249" {
		t.Fatalf("default port = %q, want %q", got, "7249")
	}
	cfg.NativePublic.H3 = "https://meter.example:18444"
	if got := publicH3Port(&cfg); got != "18444" {
		t.Fatalf("public port = %q, want %q", got, "18444")
	}
	cfg.NativePublic.H3 = "https://meter.example"
	if got := publicH3Port(&cfg); got != "443" {
		t.Fatalf("default TLS port = %q, want %q", got, "443")
	}
}

// The idle bound is a contract value: clients pace their traffic under it.
func TestEndpointsCarryThePublishedIdleBound(t *testing.T) {
	cfg := config.Default()
	e, err := buildEndpoints(t.Context(), &cfg)
	if err != nil {
		t.Fatal(err)
	}
	// Equality matters because a larger bound can outlive the upload aggregate TTL.
	if e.wtIdleBound != wire.WTIdleBound {
		t.Errorf("WebTransport idle bound = %v, want the published %v: the upload aggregate TTL is derived from that constant, so a longer bound resets the upload counter to zero across a reconnect", e.wtIdleBound, wire.WTIdleBound)
	}
}

func TestH3QUICConfigCarriesTheSupportedTransferEnvelope(t *testing.T) {
	cfg := h3QUICConfig()
	if want := int64(257); cfg.MaxIncomingStreams != want {
		t.Fatalf("incoming request streams = %d, want %d (128 download + 128 upload + progress)", cfg.MaxIncomingStreams, want)
	}
	// The literal, not the production expression: repeating that expression here asserts only that it equals itself.
	if want := int64(23); cfg.MaxIncomingUniStreams != want {
		t.Fatalf("incoming unidirectional streams = %d, want %d (3 HTTP/3 control + 16 lanes + 4 headroom)", cfg.MaxIncomingUniStreams, want)
	}
}

// api/wire.md promises the lane past wire.WTMaxStreams is reset rather than served.
func TestH3UniStreamCreditOutrunsABrowsersLaneCeiling(t *testing.T) {
	credit := h3QUICConfig().MaxIncomingUniStreams
	floor := int64(browserH3UniStreams + wire.WTMaxStreams)
	if credit <= floor {
		t.Fatalf("MaxIncomingUniStreams = %d, want more than %d (%d HTTP/3 streams a browser has already spent + the %d lane ceiling), so the app-level guard refuses the %dth lane rather than stream credit parking it",
			credit, floor, browserH3UniStreams, wire.WTMaxStreams, wire.WTMaxStreams+1)
	}
}

// The unit half of TestWebTransportConnectRefusesAForeignOrigin.
func TestWTOriginCheckPinsTheCanonicalOriginUnderAuthentication(t *testing.T) {
	authn := testPasswordAuth(t, "https://meter.example")
	withOrigin := func(origin string) *http.Request {
		r := httptest.NewRequest(http.MethodConnect, "/wt/ping", nil)
		if origin != "" {
			r.Header.Set("Origin", origin)
		}
		return r
	}
	check := wtOriginCheck(authn)
	for _, tc := range []struct {
		origin string
		want   bool
	}{
		// No Origin at all is a native client, which no browser origin policy governs; the credential is what admits it.
		{"", true},
		{authn.PublicOrigin(), true},
		{"https://attacker.example", false},
		// Neither a suffix nor a prefix of the canonical origin is it.
		{"https://meter.example.attacker.example", false},
		{"https://meter.example.evil", false},
		// The scheme is part of an origin.
		{"http://meter.example", false},
		{"null", false},
	} {
		if got := check(withOrigin(tc.origin)); got != tc.want {
			t.Errorf("wtOriginCheck(Origin: %q) = %v, want %v", tc.origin, got, tc.want)
		}
	}
	// Public mode holds no session state a forged origin could reach.
	if open := wtOriginCheck(nil); !open(withOrigin("https://attacker.example")) {
		t.Error("public mode refused a cross-origin CONNECT")
	}
}

// The admission log is the operator's whole view of what is refusing traffic.
func TestAdmissionLogLineNamesTheSessionBudget(t *testing.T) {
	a := newRequestAdmission(256, 32, 64, 16, time.Minute, time.Hour)
	for i := range 3 {
		release, status := a.acquire("client", "login-"+strconv.Itoa(i))
		if status != 0 {
			t.Fatalf("session %d rejected with %d", i, status)
		}
		defer release()
	}
	line := admissionLogLine(a.stats(), admissionStats{})
	for _, want := range []string{"sessions 3 active", "64 max"} {
		if !strings.Contains(line, want) {
			t.Errorf("admission log line %q does not report %q", line, want)
		}
	}

	full := newRequestAdmission(4, 4, 1, 4, time.Minute, time.Hour)
	release, status := full.acquire("client-a", "login-a")
	if status != 0 {
		t.Fatalf("first session rejected with %d", status)
	}
	defer release()
	if _, status := full.acquire("client-b", "login-b"); status != http.StatusServiceUnavailable {
		t.Fatalf("session past the budget = %d, want %d", status, http.StatusServiceUnavailable)
	}
	if line := admissionLogLine(full.stats(), admissionStats{}); !strings.Contains(line, "1 budget") {
		t.Errorf("admission log line %q does not distinguish a session-budget refusal from a full pool", line)
	}
}

func TestServeHandlesRequestsOverAnExplicitListener(t *testing.T) {
	ln, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatalf("net.Listen: %v", err)
	}

	mux := http.NewServeMux()
	mux.HandleFunc("/ping", func(w http.ResponseWriter, r *http.Request) {
		_, _ = io.WriteString(w, "pong")
	})
	srv := &http.Server{Handler: mux}

	done := make(chan error, 1)
	go func() { done <- serve(ln, srv) }()

	resp, err := http.Get("http://" + ln.Addr().String() + "/ping")
	if err != nil {
		t.Fatalf("GET /ping: %v", err)
	}
	body, _ := io.ReadAll(resp.Body)
	resp.Body.Close()
	if string(body) != "pong" {
		t.Fatalf("body = %q, want %q", body, "pong")
	}

	if err := ln.Close(); err != nil {
		t.Fatalf("close listener: %v", err)
	}

	select {
	case err := <-done:
		if err == nil || !errors.Is(err, net.ErrClosed) {
			t.Fatalf("serve returned %v, want a closed-listener error", err)
		}
	case <-time.After(2 * time.Second):
		t.Fatal("serve did not return after the listener closed")
	}
}

func TestRunServicesStopsEveryServiceOnCancel(t *testing.T) {
	ctx, cancel := context.WithCancel(t.Context())
	blockA, blockB := make(chan struct{}), make(chan struct{})
	stoppedA, stoppedB := false, false
	services := []service{
		{name: "a", addr: ":1", network: "tcp", run: func() error { <-blockA; return nil }, stop: func(context.Context) error { stoppedA = true; close(blockA); return nil }},
		{name: "b", addr: ":2", network: "tcp", run: func() error { <-blockB; return nil }, stop: func(context.Context) error { stoppedB = true; close(blockB); return nil }},
	}

	done := make(chan error, 1)
	go func() { done <- runServices(ctx, &config.Config{}, services) }()
	cancel()

	select {
	case err := <-done:
		if err != nil {
			t.Fatalf("clean shutdown returned %v", err)
		}
	case <-time.After(2 * time.Second):
		t.Fatal("runServices did not return after the context was cancelled")
	}
	if !stoppedA || !stoppedB {
		t.Fatalf("stop not called on every service: a=%v b=%v", stoppedA, stoppedB)
	}
}

func TestRunServicesReturnsAndStopsOnListenerError(t *testing.T) {
	boom := errors.New("bind failed")
	block := make(chan struct{})
	survivorStopped := false
	services := []service{
		{name: "bad", addr: ":1", network: "tcp", run: func() error { return boom }, stop: func(context.Context) error { return nil }},
		{name: "good", addr: ":2", network: "tcp", run: func() error { <-block; return nil }, stop: func(context.Context) error { survivorStopped = true; close(block); return nil }},
	}

	done := make(chan error, 1)
	go func() { done <- runServices(t.Context(), &config.Config{}, services) }()

	select {
	case err := <-done:
		if !errors.Is(err, boom) {
			t.Fatalf("runServices returned %v, want it to wrap the bind error", err)
		}
	case <-time.After(2 * time.Second):
		t.Fatal("runServices did not return after a listener failed")
	}
	if !survivorStopped {
		t.Fatal("a listener failure did not shut the surviving service down")
	}
}

func TestAdmissionWrapsMountedMeasurementRoutes(t *testing.T) {
	e := testEndpoints(t)
	e.admission = newRequestAdmission(1, 2, 1, 2, time.Minute, time.Hour)
	release, status := e.admission.acquire("occupied", "")
	if status != 0 {
		t.Fatalf("occupy slot: %d", status)
	}
	defer release()
	h := listenerMuxConfigured(t.Context(), e, muxTopology{discovery: true, latency: true, transfers: true, wt: &webtransport.Server{}}, nil, nil)
	for _, path := range []string{"/download", "/upload", "/upload/progress", "/ws/ping", "/wt/download", "/wt/upload", "/wt/ping"} {
		w := httptest.NewRecorder()
		h.ServeHTTP(w, httptest.NewRequest(http.MethodGet, path, nil))
		if w.Code != http.StatusServiceUnavailable || w.Header().Get("Retry-After") != "1" {
			t.Errorf("saturated %s = %d, want admission refusal", path, w.Code)
		}
	}
	for method, paths := range map[string][]string{
		http.MethodGet:     {"/preflight", "/probe"},
		http.MethodPost:    {"/upload/session", "/wt/session"},
		http.MethodOptions: {"/download", "/upload", "/upload/progress"},
	} {
		for _, path := range paths {
			w := httptest.NewRecorder()
			h.ServeHTTP(w, httptest.NewRequest(method, path, nil))
			want := http.StatusOK
			if method == http.MethodOptions {
				want = http.StatusNoContent
			}
			if w.Code != want {
				t.Errorf("unmetered %s %s = %d, want %d", method, path, w.Code, want)
			}
		}
	}
	bootstrap := listenerMuxConfigured(t.Context(), e, muxTopology{bootstrap: true}, nil, nil)
	for _, path := range []string{"/download", "/ws/ping", "/wt/upload"} {
		w := httptest.NewRecorder()
		bootstrap.ServeHTTP(w, httptest.NewRequest(http.MethodGet, path, nil))
		if w.Code != http.StatusNotFound {
			t.Errorf("unmounted %s = %d, want 404", path, w.Code)
		}
	}
}

func TestRouteMetadataPreservesPublicHEADHandling(t *testing.T) {
	h := listenerMuxConfigured(t.Context(), testEndpoints(t), muxTopology{discovery: true, transfers: true}, nil, nil)
	for _, path := range []string{"/preflight", "/probe", "/download?bytes=0"} {
		w := httptest.NewRecorder()
		h.ServeHTTP(w, httptest.NewRequest(http.MethodHead, path, nil))
		if w.Code != http.StatusOK {
			t.Errorf("HEAD %s = %d, want 200", path, w.Code)
		}
	}
}
