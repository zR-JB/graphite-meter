package server

import (
	"bufio"
	"context"
	"crypto/tls"
	"encoding/binary"
	"errors"
	"io"
	"net"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/quic-go/webtransport-go"
	"github.com/zR-JB/graphite-meter/go/internal/auth"
	"github.com/zR-JB/graphite-meter/go/internal/config"
	"github.com/zR-JB/graphite-meter/go/internal/testkit"
)

// An HTTP/2 upload's rate is bounded by the receive window per round trip, so the server advertises larger ones.
func TestHTTP2AdvertisesTheUploadReceiveWindows(t *testing.T) {
	srv := httptest.NewUnstartedServer(http.NotFoundHandler())
	srv.Config = baseServer(http.NotFoundHandler(), nil, controlTimeout)
	srv.EnableHTTP2 = true
	srv.StartTLS()
	defer srv.Close()
	conn, err := tls.Dial("tcp", srv.Listener.Addr().String(),
		&tls.Config{InsecureSkipVerify: true, NextProtos: []string{"h2"}}) //nolint:gosec // test certificate
	if err != nil {
		t.Fatal(err)
	}
	defer conn.Close()
	_ = conn.SetDeadline(time.Now().Add(5 * time.Second))
	// The client preface and an empty SETTINGS frame.
	if _, err := conn.Write(append([]byte("PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n"), 0, 0, 0, 4, 0, 0, 0, 0, 0)); err != nil {
		t.Fatal(err)
	}
	var streamWindow, connectionWindow, frameSize uint32
	for streamWindow == 0 || connectionWindow == 0 {
		var header [9]byte
		if _, err := io.ReadFull(conn, header[:]); err != nil {
			t.Fatalf("read frame (stream window %d, connection window %d): %v", streamWindow, connectionWindow, err)
		}
		payload := make([]byte, int(header[0])<<16|int(header[1])<<8|int(header[2]))
		if _, err := io.ReadFull(conn, payload); err != nil {
			t.Fatal(err)
		}
		switch frameType, stream := header[3], binary.BigEndian.Uint32(header[5:])&0x7fffffff; {
		case frameType == 0x4 && header[4]&0x1 == 0: // SETTINGS
			for setting := payload; len(setting) >= 6; setting = setting[6:] {
				switch value := binary.BigEndian.Uint32(setting[2:]); binary.BigEndian.Uint16(setting) {
				case 0x4: // SETTINGS_INITIAL_WINDOW_SIZE
					streamWindow = value
				case 0x5: // SETTINGS_MAX_FRAME_SIZE
					frameSize = value
				}
			}
		case frameType == 0x8 && stream == 0: // connection WINDOW_UPDATE
			connectionWindow = 65535 + binary.BigEndian.Uint32(payload)&0x7fffffff
		}
	}
	// A small frame bound keeps control requests from queueing behind one indivisible upload frame.
	if streamWindow != h2ReceiveWindowPerStream || connectionWindow != h2ReceiveWindowPerConnection ||
		frameSize != 16<<10 {
		t.Fatalf("advertised stream/connection windows %d/%d and frame size %d, want %d/%d and %d", streamWindow,
			connectionWindow, frameSize, h2ReceiveWindowPerStream, h2ReceiveWindowPerConnection, 16<<10)
	}
}

// Each listener mounts only its topology's routes; a dot segment never reaches the shell.
func TestListenerTopologies(t *testing.T) {
	e := testEndpoints(t)
	shell := http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) { _, _ = w.Write([]byte("shell")) })
	ui := muxTopology{spa: true, discovery: true, latency: true, transfers: true}
	for _, tc := range []struct {
		name    string
		topo    muxTopology
		proto   int
		mounted []string
		absent  []string
	}{
		{"h1 ui", ui, 1, []string{"/", "/preflight", "/ws/ping"},
			[]string{"/foo/..", "/assets/..", "/foo/%2e%2e", `/foo\..\bar`}},
		{"h1 tls", muxTopology{discovery: true, latency: true, transfers: true, requiredProto: 1}, 1,
			[]string{"/ws/ping", "/download?bytes=1"}, nil},
		{"h2", muxTopology{transfers: true, requiredProto: 2}, 2,
			[]string{"/probe", "/download?bytes=1", "/upload/session", "/upload", "/upload/progress"},
			[]string{"/", "/assets/app.js", "/preflight", "/ws/ping"}},
		{"h2 over h1", muxTopology{transfers: true, requiredProto: 2}, 1, nil,
			[]string{"/download?bytes=1", "/ws/ping"}},
		{"h3", muxTopology{transfers: true}, 3, []string{"/upload/progress?id=unknown"}, nil},
		{"h3 bootstrap", muxTopology{bootstrap: true, control: true}, 1,
			[]string{"/probe", "/upload/session", "/upload/checkpoint", "/upload/progress?id=unknown", "/wt/session"},
			[]string{"/download", "/upload", "/preflight", "/ws/ping", "/wt/upload"}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			var spa http.Handler
			if tc.topo.spa {
				spa = shell
			}
			mux := publicMux(t, e, tc.topo, spa)
			serve := func(path string) *httptest.ResponseRecorder {
				rec := httptest.NewRecorder()
				req := httptest.NewRequest(http.MethodGet, path, nil)
				req.ProtoMajor = tc.proto
				mux.ServeHTTP(rec, req)
				return rec
			}
			for _, path := range tc.mounted {
				if rec := serve(path); rec.Code == http.StatusNotFound {
					t.Errorf("%s is not mounted", path)
				}
			}
			for _, path := range tc.absent {
				if rec := serve(path); rec.Code != http.StatusNotFound || strings.Contains(rec.Body.String(), "shell") {
					t.Errorf("%s = %d %q, want 404", path, rec.Code, rec.Body.String())
				}
			}
		})
	}
}

// A client that declares a body and goes silent cannot hold its connection, before or after the handler answers.
func TestUnreadBodiesCannotHoldAConnection(t *testing.T) {
	t.Parallel()
	_, httpBase, _ := wtServer(t, nil, func(e *endpoints) { e.controlTimeout = 200 * time.Millisecond })
	for _, request := range []string{"POST /upload/session", "GET /probe", "GET /"} {
		t.Run(request, func(t *testing.T) {
			t.Parallel()
			conn, err := net.Dial("tcp", strings.TrimPrefix(httpBase, "http://"))
			if err != nil {
				t.Fatal(err)
			}
			defer conn.Close()
			sent := time.Now()
			_, _ = io.WriteString(conn, request+" HTTP/1.1\r\nHost: meter\r\nContent-Length: 200000\r\n\r\npartial")
			_ = conn.SetReadDeadline(time.Now().Add(5 * time.Second))
			res, err := http.ReadResponse(bufio.NewReader(conn), nil)
			if err != nil {
				t.Fatal(err)
			}
			_, _ = io.Copy(io.Discard, res.Body)
			if strings.HasPrefix(request, "GET") && res.StatusCode != http.StatusBadRequest {
				t.Fatalf("%s with a body = %d, want it refused before any handler", request, res.StatusCode)
			}
			// The FIN precedes the server's lingering close, so it arrives with the drain deadline.
			_, err = conn.Read(make([]byte, 1))
			if open := time.Since(sent); !errors.Is(err, io.EOF) || open > 450*time.Millisecond {
				t.Fatalf("answered %d, then the connection stayed open for %v: %v", res.StatusCode, open, err)
			}
		})
	}
}

// A header block past the listener's bound is refused before routing; one within it is served.
func TestListenerBoundsTheRequestHeaderBlock(t *testing.T) {
	t.Parallel()
	_, httpBase, _ := wtServer(t, nil, nil)
	for size, want := range map[int]int{16 << 10: http.StatusOK, 64 << 10: http.StatusRequestHeaderFieldsTooLarge} {
		req, _ := http.NewRequest(http.MethodGet, httpBase+"/preflight", nil)
		req.Header.Set("X-Padding", strings.Repeat("a", size))
		res, err := http.DefaultClient.Do(req)
		if err != nil {
			t.Fatal(err)
		}
		_, _ = io.Copy(io.Discard, res.Body)
		res.Body.Close()
		if res.StatusCode != want {
			t.Errorf("%d-byte header = %d, want %d", size, res.StatusCode, want)
		}
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

// Services drain together, each with the whole shutdown budget, whether a cancel or a failed listener ends them.
func TestRunServicesStopsEveryServiceTogether(t *testing.T) {
	boom := errors.New("bind failed")
	for _, failure := range []error{nil, boom} {
		ctx, cancel := context.WithCancel(t.Context())
		var draining sync.WaitGroup
		draining.Add(2)
		serving := func(name string, err error) service {
			block := make(chan struct{})
			return service{name: name, run: func() error {
				if err == nil {
					<-block
				}
				return err
			}, stop: func(context.Context) error {
				draining.Done()
				draining.Wait()
				close(block)
				return nil
			}}
		}
		done := make(chan error, 1)
		go func() {
			done <- runServices(ctx, &config.Config{}, []service{serving("a", nil), serving("b", failure)})
		}()
		if failure == nil {
			cancel()
		}
		select {
		case err := <-done:
			if !errors.Is(err, failure) {
				t.Fatalf("runServices returned %v, want %v", err, failure)
			}
		case <-time.After(2 * time.Second):
			t.Fatalf("runServices ended by %v did not stop its services together", failure)
		}
		cancel()
	}
}

func TestAdmissionWrapsMountedMeasurementRoutes(t *testing.T) {
	e := testEndpoints(t)
	e.admission = newRequestAdmission(1, 2, 1, 2, time.Minute, time.Hour)
	release, status := e.admission.acquire(false, "occupied")
	if status != 0 {
		t.Fatalf("occupy slot: %d", status)
	}
	defer release()
	h := publicMux(t, e, muxTopology{discovery: true, latency: true, transfers: true, wt: &webtransport.Server{}}, nil)
	for _, route := range []struct{ method, path string }{
		{http.MethodGet, "/download"}, {http.MethodPost, "/upload"}, {http.MethodGet, "/upload/progress"},
		{http.MethodDelete, "/upload/progress"},
		{http.MethodGet, "/ws/ping"}, {http.MethodConnect, "/wt/download"}, {http.MethodConnect, "/wt/upload"},
		{http.MethodConnect, "/wt/ping"},
	} {
		w := testkit.Record(h.ServeHTTP, httptest.NewRequest(route.method, route.path, nil))
		if w.Code != http.StatusServiceUnavailable || w.Header().Get("Retry-After") != "1" ||
			w.Header().Get("Access-Control-Allow-Origin") != "*" {
			t.Errorf("saturated %s %s = %d, want a readable admission refusal", route.method, route.path, w.Code)
		}
	}
	for method, paths := range map[string][]string{
		http.MethodGet:     {"/preflight", "/probe"},
		http.MethodPost:    {"/upload/session", "/wt/session"},
		http.MethodOptions: {"/download", "/upload", "/upload/progress"},
	} {
		for _, path := range paths {
			w := testkit.Record(h.ServeHTTP, httptest.NewRequest(method, path, nil))
			want := http.StatusOK
			if method == http.MethodOptions {
				want = http.StatusNoContent
			}
			if w.Code != want {
				t.Errorf("unmetered %s %s = %d, want %d", method, path, w.Code, want)
			}
		}
	}
}

// Run's own TCP listeners refuse an unauthenticated measurement, whatever each one mounts.
func TestAssembledTCPListenersEnforceAuthentication(t *testing.T) {
	hash, err := auth.HashPassword("secret")
	if err != nil {
		t.Fatal(err)
	}
	cfg := config.Default()
	cfg.Native = config.NativeEndpoints{H1: "h1", H1TLS: "h1tls", H2: "h2"}
	cfg.AdvertisedNative = map[string]bool{}
	cfg.Public.Throughput = []string{"https://meter.example"}
	cfg.Auth = config.AuthConfig{Mode: "password", PublicURL: "https://meter.example", PasswordHash: hash,
		OIDCProviderName: "Authelia"}
	_, sockets := pipeServer(t, &cfg, nil)
	for _, tc := range []struct {
		addr, scheme string
		http2        bool
	}{
		{"h1", "http", false},
		{"h1tls", "https", false},
		{"h2", "https", true},
	} {
		client := sockets[tc.addr].client(t)
		tr := client.Transport.(*http.Transport)
		tr.TLSClientConfig = &tls.Config{InsecureSkipVerify: true} //nolint:gosec // test certificate
		tr.Protocols = &http.Protocols{}
		tr.Protocols.SetHTTP1(!tc.http2)
		tr.Protocols.SetHTTP2(tc.http2)
		res, err := client.Get(tc.scheme + "://meter.example/download?bytes=1")
		if err != nil {
			t.Fatalf("%s: %v", tc.addr, err)
		}
		body, _ := io.ReadAll(res.Body)
		res.Body.Close()
		if res.StatusCode != http.StatusForbidden || len(body) == 1 {
			t.Errorf("%s unauthenticated download = %d with %d bytes, want 403", tc.addr, res.StatusCode, len(body))
		}
	}
}
