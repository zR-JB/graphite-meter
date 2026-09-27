package server

import (
	"context"
	"crypto/tls"
	"encoding/json/v2"
	"io"
	"net/http"
	"testing"
	"time"

	"github.com/zR-JB/graphite-meter/go/internal/config"
	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

func protocolTestTLS(t *testing.T) (*config.Config, *certificateManager) {
	t.Helper()
	now := time.Now()
	cert, key := writeCertificate(t, t.TempDir(), "server", "meter.example", now.Add(-time.Hour), now.Add(time.Hour))
	cfg := config.Default()
	cfg.TLSCert, cfg.TLSKey = cert, key
	cm, err := newCertificateManager(&cfg)
	if err != nil {
		t.Fatal(err)
	}
	return &cfg, cm
}

// Each native TLS listener, as Run assembles it, speaks only TLS 1.3 and its own protocol, and mounts its own routes.
func TestNativeListeners(t *testing.T) {
	t.Parallel()
	cfg, _ := startListeners(t, func(cfg *config.Config, sockets *testListenerSockets) {
		cfg.Native.H1, cfg.Native.H1TLS = sockets.reserveTCP(), sockets.reserveTCP()
		cfg.Native.H2, cfg.Native.H3 = sockets.reserveTCP(), sockets.reserveH3()
	}, nil)
	h2, h2Base := insecureClient(t, "http2"), "https://"+cfg.Native.H2
	for _, tc := range []struct {
		protocol string
		client   *http.Client
		base     string
		absent   []string
	}{
		{"http/1.1", insecureClient(t, "http1"), "https://" + cfg.Native.H1TLS, nil},
		{"h2", h2, h2Base, []string{"/", "/assets/app.js", "/preflight", "/ws/ping"}},
		{"h3", insecureClient(t, "http3"), "https://" + cfg.Native.H3, nil},
	} {
		t.Run(tc.protocol, func(t *testing.T) {
			for _, path := range tc.absent {
				res, err := tc.client.Get(tc.base + path)
				if err != nil {
					t.Fatal(err)
				}
				res.Body.Close()
				if res.StatusCode != http.StatusNotFound {
					t.Fatalf("%s = %d, want 404", path, res.StatusCode)
				}
			}
			res, err := tc.client.Get(tc.base + "/probe")
			if err != nil {
				t.Fatal(err)
			}
			var probe wire.Probe
			err = json.UnmarshalRead(res.Body, &probe)
			res.Body.Close()
			if err != nil || probe.ProtocolNegotiated != tc.protocol {
				t.Fatalf("probe negotiated %q, %v", probe.ProtocolNegotiated, err)
			}
			res, err = tc.client.Get(tc.base + "/download?bytes=1")
			if err != nil {
				t.Fatal(err)
			}
			body, _ := io.ReadAll(res.Body)
			res.Body.Close()
			if len(body) != 1 {
				t.Fatalf("download bytes = %d, want 1", len(body))
			}
		})
	}
	t.Run("TLS 1.2", func(t *testing.T) {
		conn, err := tls.Dial("tcp", cfg.Native.H1TLS,
			&tls.Config{InsecureSkipVerify: true, MaxVersion: tls.VersionTLS12}) //nolint:gosec // test certificate
		if err == nil {
			conn.Close()
			t.Fatal("a TLS 1.2 handshake succeeded")
		}
	})
	t.Run("h2 held routes refuse a body", func(t *testing.T) {
		res, err := h2.Post(h2Base+"/upload/session", "", nil)
		if err != nil {
			t.Fatal(err)
		}
		var session struct {
			UploadID string `json:"uploadId"`
		}
		err = json.UnmarshalRead(res.Body, &session)
		res.Body.Close()
		if err != nil {
			t.Fatal(err)
		}
		for _, target := range []string{"/upload/progress?id=" + session.UploadID, "/download?bytes=1000000000"} {
			body, unread := io.Pipe()
			t.Cleanup(func() { _ = unread.Close() })
			go func() { _, _ = unread.Write(make([]byte, 1<<20)) }()
			ctx, cancel := context.WithTimeout(t.Context(), 5*time.Second)
			req, _ := http.NewRequestWithContext(ctx, http.MethodGet, h2Base+target, body)
			res, err := h2.Do(req)
			if err != nil {
				cancel()
				t.Fatal(err)
			}
			res.Body.Close()
			cancel()
			if res.StatusCode != http.StatusBadRequest {
				t.Fatalf("GET %s with a body = %d, want the stream refused", target, res.StatusCode)
			}
		}
		req, _ := http.NewRequestWithContext(t.Context(), http.MethodDelete,
			h2Base+"/upload/progress?id="+session.UploadID, nil)
		res, err = h2.Do(req)
		if err != nil {
			t.Fatal(err)
		}
		res.Body.Close()
		if res.Header.Get("X-Graphite-Upload-Refusal") != "invalid" {
			t.Fatalf("bodiless DELETE did not reach the receiver store: %d", res.StatusCode)
		}
	})
}
