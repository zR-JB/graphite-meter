package server

import (
	"context"
	"encoding/json/v2"
	"io"
	"net"
	"net/http"
	"sync"
	"testing"
	"testing/synctest"
	"time"

	"github.com/zR-JB/graphite-meter/go/internal/config"
	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

// pipeListener serves in-memory connections, so a synctest bubble can run real HTTP exchanges on fake time.
type pipeListener struct {
	conns     chan net.Conn
	done      chan struct{}
	closeOnce sync.Once
}

// pipeConn reports a client address, as a socket does.
type pipeConn struct{ net.Conn }

func (pipeConn) RemoteAddr() net.Addr { return &net.TCPAddr{IP: net.IPv4(192, 0, 2, 1), Port: 1} }

func (l *pipeListener) Accept() (net.Conn, error) {
	select {
	case c := <-l.conns:
		return c, nil
	case <-l.done:
		return nil, net.ErrClosed
	}
}

func (l *pipeListener) Close() error {
	l.closeOnce.Do(func() { close(l.done) })
	return nil
}

func (l *pipeListener) Addr() net.Addr { return pipeConn{}.RemoteAddr() }

// laneServer serves the transfer routes over pipes and returns a client for them.
func laneServer(t *testing.T, operation time.Duration) (*endpoints, *http.Client) {
	ctx, cancel := context.WithCancel(t.Context())
	cfg := config.Default()
	cfg.MaxOperationDuration = operation
	e := buildEndpoints(ctx, &cfg)
	ln := &pipeListener{conns: make(chan net.Conn), done: make(chan struct{})}
	srv := &http.Server{Handler: newMux(ctx, e, muxTopology{transfers: true}, nil, publicAuth(t))}
	go func() { _ = srv.Serve(ln) }()
	tr := &http.Transport{DialContext: func(ctx context.Context, _, _ string) (net.Conn, error) {
		client, server := net.Pipe()
		select {
		case ln.conns <- pipeConn{server}:
			return client, nil
		case <-ctx.Done():
			return nil, ctx.Err()
		}
	}}
	t.Cleanup(func() {
		cancel()
		_ = srv.Close()
		tr.CloseIdleConnections()
	})
	return e, &http.Client{Transport: tr}
}

// A stalled upload lane is answered 408 once idle for the bound and keeps its bytes, as does one that reaches its
// lifetime while moving.
func TestHTTPUploadLaneEndings(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		e, client := laneServer(t, 45*time.Second)
		upload := func(send func(w io.Writer)) (res *http.Response, id string, took time.Duration, err error) {
			id = e.upload.Mint()
			body, w := io.Pipe()
			sent := make(chan struct{})
			go func() {
				defer close(sent)
				send(w)
				w.Close()
			}()
			start := time.Now()
			res, err = client.Post("http://meter/upload?id="+id, "application/octet-stream", body)
			took = time.Since(start)
			body.Close()
			<-sent
			return res, id, took, err
		}
		counted := func(id string) int64 {
			check, err := client.Post("http://meter/upload/checkpoint?id="+id, "", nil)
			if err != nil {
				t.Fatal(err)
			}
			defer check.Body.Close()
			var counters struct {
				Bytes int64 `json:"bytes"`
			}
			_ = json.UnmarshalRead(check.Body, &counters)
			return counters.Bytes
		}
		res, id, _, err := upload(func(w io.Writer) {
			_, _ = w.Write(make([]byte, 1024))
			time.Sleep(wire.WTIdleBound + time.Second)
		})
		if err != nil || res.StatusCode != http.StatusRequestTimeout ||
			res.Header.Get("X-Graphite-Upload-Refusal") != "idle" || counted(id) != 1024 {
			t.Fatalf("stalled lane = %v %v, want 408 idle keeping its 1024 bytes", res, err)
		}
		res.Body.Close()
		// The lifetime is also the answer's write deadline, so the lane closes, answered or not.
		res, id, took, err := upload(func(w io.Writer) {
			for range 6 {
				if _, err := w.Write([]byte("x")); err != nil {
					return
				}
				time.Sleep(10 * time.Second)
			}
		})
		if err == nil {
			res.Body.Close()
		}
		if took > 50*time.Second || counted(id) != 5 {
			t.Fatalf("lane at its lifetime ended after %v, want at 45s keeping the 5 bytes sent before it", took)
		}
	})
}

// A download whose reader stops frees its slot after the idle bound, while a slow steady reader keeps it.
func TestHTTPDownloadIdleBound(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		e, client := laneServer(t, 5*time.Minute)
		active := func() int {
			requests, _ := e.admission.stats()
			return requests.active
		}
		res, err := client.Get("http://meter/download?bytes=1073741824")
		if err != nil {
			t.Fatal(err)
		}
		time.Sleep(wire.WTIdleBound - time.Second)
		synctest.Wait()
		if active() != 1 {
			t.Fatal("download ended before its idle bound")
		}
		time.Sleep(2 * time.Second)
		synctest.Wait()
		if active() != 0 {
			t.Fatal("a download nobody reads held its slot past the idle bound")
		}
		res.Body.Close()

		res, err = client.Get("http://meter/download?bytes=1073741824")
		if err != nil {
			t.Fatal(err)
		}
		defer res.Body.Close()
		buf := make([]byte, 64<<10)
		for range 20 {
			if _, err := io.ReadFull(res.Body, buf); err != nil {
				t.Fatalf("a slow steady reader was cut off: %v", err)
			}
			time.Sleep(5 * time.Second)
		}
	})
}
