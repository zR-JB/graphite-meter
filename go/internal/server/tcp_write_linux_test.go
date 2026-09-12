package server

import (
	"context"
	"crypto/tls"
	"io"
	"net"
	"net/http"
	"net/http/httptest"
	"testing"
	"time"
)

// Model a receiver that drains TCP at a bounded rate. Keeping its receive
// buffer small makes the server-side unsent queue the dominant source of delay.
type pacedReadConn struct{ net.Conn }

func (c pacedReadConn) Read(p []byte) (int, error) {
	time.Sleep(8 * time.Millisecond)
	return c.Conn.Read(p[:min(len(p), 16<<10)])
}

func TestHTTP2ControlIsNotTrappedBehindQueuedDownloads(t *testing.T) {
	handler := http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path == "/download" {
			_, _ = w.Write(make([]byte, 16<<20))
			return
		}
		_, _ = w.Write([]byte("ok"))
	})
	srv := httptest.NewUnstartedServer(handler)
	protocols := new(http.Protocols)
	protocols.SetHTTP2(true)
	srv.Config = baseServer(handler, protocols)
	srv.Listener = admittedListener{Listener: srv.Listener, admission: newConnectionAdmission(10, 10, nil)}
	srv.EnableHTTP2 = true
	srv.StartTLS()
	defer srv.Close()
	tr := &http.Transport{
		ForceAttemptHTTP2: true,
		TLSClientConfig:   &tls.Config{InsecureSkipVerify: true}, //nolint:gosec
		MaxConnsPerHost:   1,
		HTTP2:             &http.HTTP2Config{MaxReadFrameSize: 16 << 10},
		DialContext: func(ctx context.Context, network, address string) (net.Conn, error) {
			c, err := (&net.Dialer{}).DialContext(ctx, network, address)
			if err != nil {
				return nil, err
			}
			if err := c.(*net.TCPConn).SetReadBuffer(64 << 10); err != nil {
				c.Close()
				return nil, err
			}
			return pacedReadConn{c}, nil
		},
	}
	defer tr.CloseIdleConnections()
	client := &http.Client{Transport: tr}
	ctx, cancel := context.WithCancel(t.Context())
	defer cancel()
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, srv.URL+"/download", nil)
	if err != nil {
		t.Fatal(err)
	}
	download, err := client.Do(req)
	if err != nil {
		t.Fatal(err)
	}
	if download.ProtoMajor != 2 {
		t.Fatal("fixture did not negotiate HTTP/2")
	}
	done := make(chan struct{})
	go func() { defer close(done); _, _ = io.Copy(io.Discard, download.Body) }()
	defer func() { cancel(); download.Body.Close(); <-done }()
	time.Sleep(200 * time.Millisecond)
	control, stop := context.WithTimeout(t.Context(), 500*time.Millisecond)
	defer stop()
	req, err = http.NewRequestWithContext(control, http.MethodGet, srv.URL+"/control", nil)
	if err != nil {
		t.Fatal(err)
	}
	started := time.Now()
	response, err := client.Do(req)
	if err != nil {
		t.Fatalf("control delayed by queued download: %v", err)
	}
	defer response.Body.Close()
	if _, err := io.Copy(io.Discard, response.Body); err != nil {
		t.Fatal(err)
	}
	t.Logf("control completed in %v", time.Since(started))
}
