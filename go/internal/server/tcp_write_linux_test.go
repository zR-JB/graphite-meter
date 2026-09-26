package server

import (
	"context"
	"crypto/tls"
	"io"
	"net"
	"net/http"
	"net/http/httptest"
	"sync/atomic"
	"testing"
	"time"
)

// pacedReadConn drains slowly through a small receive buffer, so the server's unsent queue fills; it counts
// what it drained and closes drained at 4 MiB, by when the server has long filled any queue it keeps.
type pacedReadConn struct {
	net.Conn
	read    *atomic.Int64
	drained chan struct{}
}

func (c pacedReadConn) Read(p []byte) (int, error) {
	time.Sleep(time.Millisecond)
	n, err := c.Conn.Read(p[:min(len(p), 16<<10)])
	if total := c.read.Add(int64(n)); total >= 4<<20 && total-int64(n) < 4<<20 {
		close(c.drained)
	}
	return n, err
}

func TestHTTP2ControlIsNotTrappedBehindQueuedDownloads(t *testing.T) {
	handler := http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path == "/download" {
			_, _ = w.Write(make([]byte, 64<<20))
			return
		}
		_, _ = w.Write([]byte("ok"))
	})
	srv := httptest.NewUnstartedServer(handler)
	protocols := new(http.Protocols)
	protocols.SetHTTP2(true)
	srv.Config = baseServer(handler, protocols, controlTimeout)
	srv.Listener = admittedListener{Listener: srv.Listener, admission: newConnectionAdmission(10, 10, nil)}
	srv.EnableHTTP2 = true
	srv.StartTLS()
	defer srv.Close()
	var read atomic.Int64
	drained := make(chan struct{})
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
			return pacedReadConn{c, &read, drained}, nil
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
	<-drained
	req, err = http.NewRequestWithContext(t.Context(), http.MethodGet, srv.URL+"/control", nil)
	if err != nil {
		t.Fatal(err)
	}
	before := read.Load()
	response, err := client.Do(req)
	if err != nil {
		t.Fatal(err)
	}
	defer response.Body.Close()
	if _, err := io.Copy(io.Discard, response.Body); err != nil {
		t.Fatal(err)
	}
	if queued := read.Load() - before; queued > 1<<20 {
		t.Fatalf("the control answer waited behind %d queued download bytes", queued)
	}
}
