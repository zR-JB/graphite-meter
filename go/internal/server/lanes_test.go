package server

import (
	"context"
	"encoding/json/v2"
	"io"
	"net/http"
	"slices"
	"strconv"
	"strings"
	"testing"
	"testing/synctest"
	"time"

	"github.com/zR-JB/graphite-meter/go/internal/apipin"
	"github.com/zR-JB/graphite-meter/go/internal/config"
	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

func laneServer(t *testing.T, operation time.Duration) (*endpoints, *http.Client) {
	cfg := config.Default()
	cfg.MaxOperationDuration = operation
	build, sockets := pipeServer(t, &cfg, nil)
	return build.e, sockets[cfg.Native.H1].client(t)
}

// Shutdown drains measurements for its grace period, then cuts the ones still open.
func TestShutdownCutsLanesThatOutliveTheDrain(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		ctx, cancel := context.WithCancel(t.Context())
		cfg := config.Default()
		ln := newPipeListener()
		stopped := make(chan error)
		go func() { stopped <- runWithSockets(ctx, &cfg, pipeSockets{cfg.Native.H1: ln}) }()
		client := ln.client(t)
		session, err := client.Post("http://meter/upload/session", "", nil)
		if err != nil {
			t.Fatal(err)
		}
		var minted struct {
			UploadID string `json:"uploadId"`
		}
		_ = json.UnmarshalRead(session.Body, &minted)
		session.Body.Close()
		body, w := io.Pipe()
		defer w.Close()
		go func() { _, _ = w.Write([]byte("x")) }()
		answered := make(chan time.Duration)
		start := time.Now()
		go func() {
			res, err := client.Post("http://meter/upload?id="+minted.UploadID, "", body)
			if err == nil {
				res.Body.Close()
			}
			answered <- time.Since(start)
		}()
		synctest.Wait()
		cancel()
		if err := <-stopped; err != nil {
			t.Fatal(err)
		}
		w.Close()
		if took := <-answered; took > 6*time.Second {
			t.Fatalf("a stalled upload outlived the shutdown drain by %v", took)
		}
		// net/http lingers half a second after closing an answered connection's write side.
		time.Sleep(time.Second)
	})
}

// pinnedRefusal checks and closes an HTTP lane refusal against api/uploadrefusals.txt.
func pinnedRefusal(t *testing.T, res *http.Response, name string) {
	t.Helper()
	defer res.Body.Close()
	body, _ := io.ReadAll(res.Body)
	rows := apipin.Rows(t, "uploadrefusals.txt", 3)
	i := slices.IndexFunc(rows, func(row []string) bool { return row[0] == name })
	if i < 0 || strconv.Itoa(res.StatusCode) != rows[i][2] || strings.TrimSpace(string(body)) != rows[i][1] ||
		res.Header.Get("X-Graphite-Upload-Refusal") != name {
		t.Fatalf("%s refusal = %d %q %v, not as pinned", name, res.StatusCode, body, res.Header)
	}
}

// An upload lane answered 408 when idle, or closed at its lifetime, keeps its bytes.
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
			time.Sleep(wire.IdleBound + time.Second)
		})
		if err != nil {
			t.Fatal(err)
		}
		pinnedRefusal(t, res, "idle")
		if counted(id) != 1024 {
			t.Fatal("the idle lane lost its 1024 bytes")
		}
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
		time.Sleep(wire.IdleBound - time.Second)
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
