package server

import (
	"bufio"
	"bytes"
	"cmp"
	"context"
	"crypto/tls"
	"encoding/binary"
	"encoding/json/v2"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/netip"
	"os"
	"slices"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"github.com/quic-go/quic-go"
	"github.com/quic-go/quic-go/http3"
	"github.com/quic-go/webtransport-go"
	"github.com/zR-JB/graphite-meter/go/internal/config"
	"github.com/zR-JB/graphite-meter/go/internal/goclient"
	"github.com/zR-JB/graphite-meter/go/internal/route"
	"github.com/zR-JB/graphite-meter/go/internal/testkit"
	"github.com/zR-JB/graphite-meter/go/internal/transport"
	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

// testWTTransport is a session transport whose close is armed only once a dial has landed.
type testWTTransport struct {
	*webtransport.Transport
	owner *testing.T
	arm   sync.Once
}

// armClose ties the transport to the test that asked for it, not to whichever subtest happened to dial first.
func (d *testWTTransport) armClose() {
	d.arm.Do(func() { d.owner.Cleanup(func() { _ = d.Transport.Close() }) })
}

// wtServer boots HTTP/1.1 and HTTP/3 listeners; shape may adjust the measurement core before it is mounted.
func wtServer(t *testing.T, tune func(*config.Config), shape func(*endpoints)) (string, string, *endpoints) {
	t.Helper()
	cfg, build := startListeners(t, func(cfg *config.Config, sockets *testListenerSockets) {
		cfg.Native.H1 = sockets.reserveTCP()
		cfg.Native.H3 = sockets.reserveH3()
		if tune != nil {
			tune(cfg)
		}
	}, shape)
	return "https://" + cfg.Native.H3, "http://" + cfg.Native.H1, build.e
}

func wtTestServer(t *testing.T, tune func(*config.Config), shape func(*endpoints)) (string, string, *testWTTransport) {
	t.Helper()
	h3Base, httpBase, _ := wtServer(t, tune, shape)
	return h3Base, httpBase, &testWTTransport{Transport: insecureWTTransport(), owner: t}
}

func idleBound(bound time.Duration) func(*endpoints) {
	return func(e *endpoints) { e.idleBound = bound }
}

// insecureWTTransport dials a test listener's self-signed certificate.
func insecureWTTransport() *webtransport.Transport {
	return &webtransport.Transport{
		TLSClientConfig: &tls.Config{InsecureSkipVerify: true}, //nolint:gosec // self-signed test certificate
		QUICConfig:      transport.NewQUICConfig(),
	}
}

// dialWebTransport returns the session, or nil and the status that refused it; the listener is bound already.
func dialWebTransport(t *testing.T, d *webtransport.Transport, target string,
	hdr http.Header) (*webtransport.Session, int) {
	t.Helper()
	ctx, cancel := context.WithTimeout(t.Context(), 10*time.Second)
	defer cancel()
	res, sess, err := d.Dial(ctx, target, hdr)
	switch {
	case err == nil:
		t.Cleanup(func() { _ = sess.CloseWithError(0, "") })
		return sess, http.StatusOK
	case res == nil:
		t.Fatalf("dial %s: %v", target, err)
	}
	return nil, res.StatusCode
}

func dialWT(t *testing.T, wtTransport *testWTTransport, url string) *webtransport.Session {
	t.Helper()
	sess, status := dialWebTransport(t, wtTransport.Transport, url, nil)
	if sess == nil {
		t.Fatalf("dial %s = %d", url, status)
	}
	wtTransport.armClose()
	return sess
}

// acceptFeed accepts an upload session's progress feed, readable for ten seconds.
func acceptFeed(t *testing.T, sess *webtransport.Session) *bufio.Scanner {
	t.Helper()
	ctx, cancel := context.WithTimeout(t.Context(), 10*time.Second)
	defer cancel()
	feed, err := sess.AcceptUniStream(ctx)
	if err != nil {
		t.Fatalf("accept progress feed: %v", err)
	}
	_ = feed.SetReadDeadline(time.Now().Add(10 * time.Second))
	return bufio.NewScanner(feed)
}

// nextRecord skips heartbeats to the feed's next record; an ended feed yields the zero record.
func nextRecord(t *testing.T, feed *bufio.Scanner) wire.UploadProgress {
	t.Helper()
	for feed.Scan() {
		if len(bytes.TrimSpace(feed.Bytes())) == 0 {
			continue
		}
		record, err := wire.DecodeUploadProgress(feed.Bytes())
		if err != nil {
			t.Fatalf("decode record %q: %v", feed.Text(), err)
		}
		return record
	}
	return wire.UploadProgress{}
}

// A download clamps its lanes to the published maximum, and each lane carries exactly the requested size.
func TestWebTransportDownloadLanes(t *testing.T) {
	t.Parallel()
	base, _, wtTransport := wtTestServer(t, nil, nil)
	clamped := dialWT(t, wtTransport, base+"/wt/download?bytes=67108864&streams=99")
	ctx, cancel := context.WithTimeout(t.Context(), 10*time.Second)
	defer cancel()
	for lane := range wire.WTMaxStreams {
		str, err := clamped.AcceptUniStream(ctx)
		if err != nil {
			t.Fatalf("lane %d: %v", lane, err)
		}
		if _, err := io.ReadFull(str, make([]byte, 1)); err != nil {
			t.Fatalf("lane %d delivered nothing: %v", lane, err)
		}
	}
	// Flow control holds every undrained lane open, so no replacement lane is due yet.
	extra, cancelExtra := context.WithTimeout(ctx, 300*time.Millisecond)
	defer cancelExtra()
	if _, err := clamped.AcceptUniStream(extra); err == nil {
		t.Fatalf("a lane past the %d-lane cap opened", wire.WTMaxStreams)
	}
	_ = clamped.CloseWithError(0, "")
	lane, err := dialWT(t, wtTransport, base+"/wt/download?bytes=1048576").AcceptUniStream(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if n, err := io.Copy(io.Discard, lane); err != nil || n != 1<<20 {
		t.Fatalf("lane served %d bytes: %v, want 1 MiB", n, err)
	}
}

// mintUploadID mints an upload id over HTTP, the half of an upload that never rides the session.
func mintUploadID(t *testing.T, httpBase string) string {
	t.Helper()
	res, err := http.DefaultClient.Post(httpBase+"/upload/session", "", nil)
	if err != nil {
		t.Fatalf("mint upload session: %v", err)
	}
	defer res.Body.Close()
	var minted struct {
		UploadID string `json:"uploadId"`
	}
	if err := json.UnmarshalRead(res.Body, &minted); err != nil {
		t.Fatalf("decode upload session: %v", err)
	}
	if minted.UploadID == "" {
		t.Fatal("upload session returned an empty id")
	}
	return minted.UploadID
}

// A lane carries more than one stream window, so only a lane the server reads can finish; the excess are reset.
func TestWebTransportUploadClampsTheLaneCount(t *testing.T) {
	t.Parallel()
	base, httpBase, wtTransport := wtTestServer(t, nil, nil)
	sess := dialWT(t, wtTransport, base+"/wt/upload?id="+mintUploadID(t, httpBase))
	const opened = wire.WTMaxStreams + 4
	var finished atomic.Int64
	var wg sync.WaitGroup
	for lane := range opened {
		openCtx, cancelOpen := context.WithTimeout(t.Context(), 10*time.Second)
		str, err := sess.OpenUniStreamSync(openCtx)
		cancelOpen()
		if err != nil {
			t.Fatalf("open lane %d: %v", lane, err)
		}
		wg.Go(func() {
			if err := str.SetWriteDeadline(time.Now().Add(10 * time.Second)); err != nil {
				t.Errorf("lane %d write deadline: %v", lane, err)
				return
			}
			_, err := io.CopyN(str, zeroes{}, 1<<20)
			if errors.Is(err, os.ErrDeadlineExceeded) {
				t.Errorf("lane %d parked on flow control instead of being read or reset", lane)
			} else if err == nil {
				finished.Add(1)
			}
		})
	}
	wg.Wait()
	if got := finished.Load(); got != wire.WTMaxStreams {
		t.Fatalf("%d of %d lanes were read, want the %d cap", got, opened, wire.WTMaxStreams)
	}
}

func TestWebTransportUploadDrainsDatagrams(t *testing.T) {
	t.Parallel()
	base, httpBase, wtTransport := wtTestServer(t, nil, nil)
	sess := dialWT(t, wtTransport, base+"/wt/upload?datagrams=1&id="+mintUploadID(t, httpBase))
	feed := acceptFeed(t, sess)
	if record := nextRecord(t, feed); record.Type != "ready" {
		t.Fatalf("first record %+v, want ready", record)
	}

	// Datagrams are lossy, so the assertion is that the drain counts them, not that every one lands.
	payload := make([]byte, 1000)
	stop := make(chan struct{})
	var sending sync.WaitGroup
	sending.Go(func() {
		for sess.SendDatagram(payload) == nil {
			select {
			case <-stop:
				return
			case <-time.After(2 * time.Millisecond):
			}
		}
	})
	defer sending.Wait()
	defer close(stop)
	for {
		switch record := nextRecord(t, feed); {
		case record.Type == "":
			t.Fatal("the feed ended before it counted a datagram")
		case record.Type == "progress" && record.Bytes > 0:
			return
		}
	}
}

func TestWebTransportDatagramFloodRepeats(t *testing.T) {
	t.Parallel()
	base, _, wtTransport := wtTestServer(t, nil, nil)
	sess := dialWT(t, wtTransport, base+"/wt/download?bytes=2000&datagrams=1")

	ctx, cancel := context.WithTimeout(t.Context(), 5*time.Second)
	defer cancel()
	for got := 0; got <= 2000; {
		d, err := sess.ReceiveDatagram(ctx)
		if err != nil {
			t.Fatalf("flood ended after %d bytes: %v", got, err)
		}
		got += len(d)
	}
}

func TestWebTransportVerifySessionLingersAndServesNothing(t *testing.T) {
	t.Parallel()
	base, _, wtTransport := wtTestServer(t, nil, nil)
	sess := dialWT(t, wtTransport, base+"/wt/download?bytes=0")

	ctx, cancel := context.WithTimeout(t.Context(), 300*time.Millisecond)
	defer cancel()
	if str, err := sess.AcceptUniStream(ctx); err == nil {
		t.Fatalf("verify session opened a stream: %v", str)
	}
	// A session that was going to be torn down at once has been by now.
	select {
	case <-sess.Context().Done():
		t.Fatal("verify session closed instead of lingering: its answer is the handshake, and the client closes it")
	default:
	}
}

func TestWebTransportSessionEndingsCarryTheirCause(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		reason string
		code   webtransport.SessionErrorCode
		tune   func(*config.Config)
		shape  func(*endpoints)
	}{
		{"idle", 1, nil, idleBound(300 * time.Millisecond)},
		{"lifetime", 2, func(c *config.Config) { c.MaxOperationDuration = 300 * time.Millisecond }, nil},
		{"shutdown", 4, nil, nil},
	} {
		t.Run(tc.reason, func(t *testing.T) {
			t.Parallel()
			sockets := newTestListenerSockets(t)
			cfg := config.Default()
			cfg.Native.H1, cfg.Native.H3 = sockets.reserveTCP(), sockets.reserveH3()
			cfg.TLSCert, cfg.TLSKey = runTestTLS(t)
			if tc.tune != nil {
				tc.tune(&cfg)
			}
			ctx, shutdown := context.WithCancel(t.Context())
			defer shutdown()
			build, err := newListenerBuild(ctx, &cfg, sockets)
			if err != nil {
				t.Fatal(err)
			}
			if tc.shape != nil {
				tc.shape(build.e)
			}
			if err := build.assemble(); err != nil {
				t.Fatal(err)
			}
			served := make(chan error, 1)
			go func() { served <- runServices(ctx, &cfg, build.services) }()
			defer func() { shutdown(); <-served }()

			dialing, cancel := context.WithTimeout(t.Context(), 10*time.Second)
			defer cancel()
			conn, err := quic.DialAddr(dialing, cfg.Native.H3, &tls.Config{InsecureSkipVerify: true,
				NextProtos: []string{http3.NextProtoH3}}, transport.NewQUICConfig()) //nolint:gosec // test certificate
			if err != nil {
				t.Fatal(err)
			}
			client, err := insecureWTTransport().NewClientConn(conn)
			if err != nil {
				t.Fatal(err)
			}
			_, sess, err := client.Dial(dialing, "https://"+cfg.Native.H3+"/wt/ping", nil)
			if err != nil {
				t.Fatal(err)
			}
			if tc.reason == "shutdown" {
				shutdown()
			}
			_, err = sess.AcceptUniStream(dialing)
			if closed, ok := errors.AsType[*webtransport.SessionError](err); !ok || closed.ErrorCode != tc.code ||
				closed.Message != tc.reason {
				t.Fatalf("session ended with %v, want %d %q", err, tc.code, tc.reason)
			}
			if tc.reason == "shutdown" {
				return
			}
			ended := time.Now()
			select {
			case <-conn.Context().Done():
				closed, ok := errors.AsType[*quic.ApplicationError](context.Cause(conn.Context()))
				if !ok || !closed.Remote || closed.ErrorCode != quic.ApplicationErrorCode(http3.ErrCodeNoError) ||
					time.Since(ended) < wtCloseLinger/2 {
					t.Fatalf("connection closed after %v with %v, want the server's H3_NO_ERROR a linger later",
						time.Since(ended), context.Cause(conn.Context()))
				}
			case <-dialing.Done():
				t.Fatal("the server kept the connection of its ended session open")
			}
		})
	}
}

// Chromium and Firefox drop the code of a close whose STOP_SENDING reaches them first; this peer reads it as they do.
func TestServerEndedSessionClosesInTheOrderBrowsersRead(t *testing.T) {
	t.Parallel()
	for _, answers := range []bool{true, false} {
		t.Run(fmt.Sprintf("peer answers %v", answers), func(t *testing.T) {
			t.Parallel()
			base, _, _ := wtServer(t, func(c *config.Config) { c.MaxOperationDuration = 300 * time.Millisecond }, nil)
			ctx, cancel := context.WithTimeout(t.Context(), 10*time.Second)
			defer cancel()
			conn, err := quic.DialAddr(ctx, strings.TrimPrefix(base, "https://"), &tls.Config{InsecureSkipVerify: true,
				NextProtos: []string{http3.NextProtoH3}}, transport.NewQUICConfig()) //nolint:gosec // test certificate
			if err != nil {
				t.Fatal(err)
			}
			defer conn.CloseWithError(0, "")
			str, err := (&http3.Transport{EnableDatagrams: true}).NewClientConn(conn).OpenRequestStream(ctx)
			if err != nil {
				t.Fatal(err)
			}
			req, _ := http.NewRequestWithContext(ctx, http.MethodConnect, base+"/wt/ping", nil)
			req.Proto = "webtransport"
			if err := str.SendRequestHeader(req); err != nil {
				t.Fatal(err)
			}
			if res, err := str.ReadResponse(); err != nil || res.StatusCode != http.StatusOK {
				t.Fatalf("CONNECT answered %v, %v", res, err)
			}

			capsules := http3.NewCapsuleParser(str)
			typ, capsule, err := capsules.Next()
			if err != nil {
				t.Fatal(err)
			}
			payload, err := io.ReadAll(capsule)
			want := append(binary.BigEndian.AppendUint32(nil, wire.LaneLifetime.WT), wire.LaneLifetime.Reason...)
			if err != nil || typ != wtCloseSessionCapsule || !bytes.Equal(payload, want) {
				t.Fatalf("capsule %#x %q, %v; want the lifetime close %q", typ, payload, err, want)
			}
			if _, _, err := capsules.Next(); err != io.EOF {
				t.Fatalf("after the capsule: %v, want the server's FIN", err)
			}
			closed := time.Now()
			if stopped := context.Cause(str.Context()); stopped != nil {
				t.Fatalf("the server stopped reading before the peer finished: %v", stopped)
			}
			if answers {
				_ = str.Close()
			} else {
				<-str.Context().Done()
				stopped, ok := errors.AsType[*quic.StreamError](context.Cause(str.Context()))
				if !ok || !stopped.Remote || stopped.ErrorCode != webtransport.WTSessionGoneErrorCode ||
					time.Since(closed) < wtCloseLinger/2 {
					t.Fatalf("stopped after %v with %v, want WT_SESSION_GONE a linger later", time.Since(closed),
						context.Cause(str.Context()))
				}
			}
			select {
			case <-conn.Context().Done():
				ended, ok := errors.AsType[*quic.ApplicationError](context.Cause(conn.Context()))
				if !ok || !ended.Remote || ended.ErrorCode != quic.ApplicationErrorCode(http3.ErrCodeNoError) {
					t.Fatalf("connection closed with %v, want the server's H3_NO_ERROR", context.Cause(conn.Context()))
				}
			case <-ctx.Done():
				t.Fatal("the server kept the connection of its ended session open")
			}
		})
	}
}

// Browsers keep a closed session's QUIC connection open, so the server must close it or hit the per-client cap.
func TestSequentialWebTransportSessionsDoNotHoldConnectionSlots(t *testing.T) {
	t.Parallel()
	base, _, wtTransport := wtTestServer(t, nil, nil)
	ctx, cancel := context.WithTimeout(t.Context(), 15*time.Second)
	defer cancel()
	tlsConfig := &tls.Config{InsecureSkipVerify: true, NextProtos: []string{http3.NextProtoH3}} //nolint:gosec
	for i := range 2 * maxClientQUICConnections {
		conn, err := quic.DialAddr(ctx, strings.TrimPrefix(base, "https://"), tlsConfig, transport.NewQUICConfig())
		if err != nil {
			t.Fatalf("connection %d refused: %v", i, err)
		}
		client, err := wtTransport.NewClientConn(conn)
		if err != nil {
			t.Fatalf("connection %d: %v", i, err)
		}
		_, sess, err := client.Dial(ctx, base+"/wt/ping", nil)
		if err != nil {
			t.Fatalf("session %d: %v", i, err)
		}
		_ = sess.CloseWithError(0, "")
		select {
		case <-conn.Context().Done():
			closed, ok := errors.AsType[*quic.ApplicationError](context.Cause(conn.Context()))
			if !ok || !closed.Remote || closed.ErrorCode != quic.ApplicationErrorCode(http3.ErrCodeNoError) {
				t.Fatalf("connection %d closed with %v, want the server's H3_NO_ERROR",
					i, context.Cause(conn.Context()))
			}
		case <-ctx.Done():
			t.Fatalf("the server kept connection %d open after its only session ended", i)
		}
	}
}

func TestShutdownClosesHTTP3ConnectionsWithH3NoError(t *testing.T) {
	t.Parallel()
	sockets := newTestListenerSockets(t)
	cfg := config.Default()
	cfg.Native.H1, cfg.Native.H3 = sockets.reserveTCP(), sockets.reserveH3()
	cfg.TLSCert, cfg.TLSKey = runTestTLS(t)
	stop := runUntilCancel(t, &cfg, sockets)
	ctx, cancel := context.WithTimeout(t.Context(), 10*time.Second)
	defer cancel()
	conn, err := quic.DialAddr(ctx, cfg.Native.H3, &tls.Config{InsecureSkipVerify: true,
		NextProtos: []string{http3.NextProtoH3}}, transport.NewQUICConfig()) //nolint:gosec // test certificate
	if err != nil {
		t.Fatal(err)
	}
	req, _ := http.NewRequestWithContext(ctx, http.MethodGet, "https://"+cfg.Native.H3+"/probe", nil)
	res, err := (&http3.Transport{}).NewClientConn(conn).RoundTrip(req)
	if err != nil {
		t.Fatal(err)
	}
	res.Body.Close()
	stop()
	select {
	case <-conn.Context().Done():
		closed, ok := errors.AsType[*quic.ApplicationError](context.Cause(conn.Context()))
		if !ok || !closed.Remote || closed.ErrorCode != quic.ApplicationErrorCode(http3.ErrCodeNoError) {
			t.Fatalf("shutdown closed the connection with %v, want H3_NO_ERROR", context.Cause(conn.Context()))
		}
	case <-ctx.Done():
		t.Fatal("shutdown left the connection open")
	}
}

// A stream download's liveness is the peer draining its lanes, and that is the only thing keeping the session open.
func TestDrainedStreamDownloadOutlivesTheIdleBound(t *testing.T) {
	t.Parallel()
	const bound = time.Second
	base, _, wtTransport := wtTestServer(t, nil, idleBound(bound))
	sess := dialWT(t, wtTransport, base+"/wt/download?bytes=262144&streams=1")

	ctx, cancel := context.WithTimeout(t.Context(), 30*time.Second)
	defer cancel()
	start := time.Now()
	// Reaping takes at most 1.5 bounds, so surviving two proves draining is what kept it.
	deadline := start.Add(2 * bound)
	var total int64
	for time.Now().Before(deadline) {
		str, err := sess.AcceptUniStream(ctx)
		if err != nil {
			t.Fatalf("the session was reaped after %v of continuous draining, %d bytes in, under a %v idle bound: %v",
				time.Since(start), total, bound, err)
		}
		n, err := io.Copy(io.Discard, str)
		total += n
		if err != nil {
			t.Fatalf("lane read failed after %v, %d bytes in: %v", time.Since(start), total, err)
		}
	}
	if total == 0 {
		t.Fatal("the session survived without carrying anything, so nothing about liveness was proved")
	}
}

// A client holds only a few QUIC connections, and each request stream may buffer only a small header block.
func TestHTTP3BoundsClientConnectionsAndHeaders(t *testing.T) {
	t.Parallel()
	h3Base, _, _ := wtServer(t, nil, nil)
	tlsConfig := &tls.Config{InsecureSkipVerify: true, NextProtos: []string{http3.NextProtoH3}} //nolint:gosec
	ctx, cancel := context.WithTimeout(t.Context(), 10*time.Second)
	defer cancel()
	dial := func() (*quic.Conn, error) {
		return quic.DialAddr(ctx, strings.TrimPrefix(h3Base, "https://"), tlsConfig, transport.NewQUICConfig())
	}
	var held []*quic.Conn
	for i := range maxClientQUICConnections {
		conn, err := dial()
		if err != nil {
			t.Fatalf("connection %d: %v", i, err)
		}
		held = append(held, conn)
	}
	if conn, err := dial(); err == nil {
		_ = conn.CloseWithError(0, "")
		t.Fatalf("connection %d from one client was admitted", maxClientQUICConnections+1)
	}
	cfg := config.Default()
	shares := cfg.MaxActiveMeasurementsPerClient + cfg.MaxSessionsPerClient
	opened := 0
	for ; opened <= shares+h3ControlStreams; opened++ {
		if _, err := held[0].OpenStream(); err != nil {
			break
		}
	}
	if opened != shares+h3ControlStreams {
		t.Fatalf("one connection opened %d request streams, want %d", opened, shares+h3ControlStreams)
	}
	for _, conn := range held {
		_ = conn.CloseWithError(0, "")
	}
	h3 := &http3.Transport{TLSClientConfig: tlsConfig, QUICConfig: transport.NewQUICConfig()}
	defer h3.Close()
	probe := func(padding int) (int, error) {
		req, _ := http.NewRequestWithContext(ctx, http.MethodGet, h3Base+route.Probe, nil)
		req.Header.Set("X-Padding", strings.Repeat("x", padding))
		res, err := h3.RoundTrip(req)
		if err != nil {
			return 0, err
		}
		_ = res.Body.Close()
		return res.StatusCode, nil
	}
	testkit.Eventually(t, 10*time.Second, "an ordinary request is served once the held connections close",
		func() bool { status, _ := probe(1024); return status == http.StatusOK })
	if status, err := probe(6 << 10); err == nil && status == http.StatusOK {
		t.Fatal("a header block over the limit was served")
	}
}

// Every WebTransport session its peer stops driving gives its slot back within the idle bound.
func TestIdleWebTransportSessionsFreeTheirSlots(t *testing.T) {
	t.Parallel()
	for name, open := range map[string]func(t *testing.T, base, httpBase string, tr *testWTTransport){
		"unread download": func(t *testing.T, base, _ string, tr *testWTTransport) {
			dialWT(t, tr, base+"/wt/download?bytes=1073741824&streams=1")
		},
		"unread datagram flood": func(t *testing.T, base, _ string, tr *testWTTransport) {
			dialWT(t, tr, base+"/wt/download?bytes=2000&datagrams=1")
		},
		"silent ping": func(t *testing.T, base, _ string, tr *testWTTransport) { dialWT(t, tr, base+route.WTPing) },
		"silent upload": func(t *testing.T, base, httpBase string, tr *testWTTransport) {
			dialWT(t, tr, base+"/wt/upload?id="+mintUploadID(t, httpBase))
		},
		"stalled upload lane": func(t *testing.T, base, httpBase string, tr *testWTTransport) {
			lane, err := dialWT(t, tr, base+"/wt/upload?id="+mintUploadID(t, httpBase)).OpenUniStreamSync(t.Context())
			if err != nil {
				t.Fatalf("open lane: %v", err)
			}
			if _, err := lane.Write(make([]byte, 4096)); err != nil {
				t.Fatalf("write lane: %v", err)
			}
		},
	} {
		t.Run(name, func(t *testing.T) {
			t.Parallel()
			base, httpBase, tr := wtTestServer(t, nil, idleBound(time.Second))
			open(t, base, httpBase, tr)
			waitForLoad(t, httpBase, 1)
			waitForLoad(t, httpBase, 0)
		})
	}
}

// A byte stream carries no channel to report a refusal on, so the refusal is the reset.
func TestRefusedWebTransportUploadLaneIsReset(t *testing.T) {
	t.Parallel()
	base, httpBase, wtTransport := wtTestServer(t, nil, nil)
	id := mintUploadID(t, httpBase)
	sess := dialWT(t, wtTransport, base+"/wt/upload?id="+id)
	// The feed's ready record is the server's word that the receiver exists.
	if record := nextRecord(t, acceptFeed(t, sess)); record.Type != "ready" {
		t.Fatalf("first record %+v, want ready", record)
	}
	// A finished receiver refuses every later lane.
	finish, _ := http.NewRequest(http.MethodDelete, httpBase+"/upload/progress?id="+id, nil)
	if res, err := http.DefaultClient.Do(finish); err != nil || res.StatusCode != http.StatusNoContent {
		t.Fatalf("finish upload: %v %v", res, err)
	}

	openCtx, cancelOpen := context.WithTimeout(t.Context(), 10*time.Second)
	defer cancelOpen()
	lane, err := sess.OpenUniStreamSync(openCtx)
	if err != nil {
		t.Fatalf("open lane: %v", err)
	}
	// The deadline is the harness's only way out: an unreset lane parks on flow control once the window fills.
	if err := lane.SetWriteDeadline(time.Now().Add(15 * time.Second)); err != nil {
		t.Fatalf("lane write deadline: %v", err)
	}
	block := make([]byte, 64<<10)
	for {
		if _, err := lane.Write(block); err != nil {
			if errors.Is(err, os.ErrDeadlineExceeded) {
				t.Fatal("a refused upload lane stayed open: the client parked on flow control, not the reset")
			}
			break
		}
	}
	// The session still says why: the refusal is a record on its own stream.
	if record := nextRecord(t, acceptFeed(t, sess)); record.Type != "error" {
		t.Fatalf("refused lane reported %+v, want its refusal record", record)
	}
}

// probeLoad reads the occupancy /probe reports. /probe is not admission-wrapped.
func probeLoad(t *testing.T, httpBase string) int {
	t.Helper()
	res, err := http.Get(httpBase + "/probe")
	if err != nil {
		t.Fatalf("probe: %v", err)
	}
	defer res.Body.Close()
	var body struct {
		Load *struct {
			Active int `json:"active"`
		} `json:"load"`
	}
	if err := json.UnmarshalRead(res.Body, &body); err != nil {
		t.Fatalf("decode probe: %v", err)
	}
	if body.Load == nil {
		t.Fatal("probe reported no load")
	}
	return body.Load.Active
}

func waitForLoad(t *testing.T, httpBase string, want int) {
	t.Helper()
	testkit.Eventually(t, 10*time.Second, fmt.Sprint("occupancy reaches ", want),
		func() bool { return probeLoad(t, httpBase) == want })
}

// An upload session joins only its own client's receiver, whoever holds the id; a refused session frees its slot.
func TestWebTransportUploadRefusesAnotherClientsReceiver(t *testing.T) {
	t.Parallel()
	base, _, e := wtServer(t, func(c *config.Config) {
		c.TrustedProxies = []netip.Prefix{netip.MustParsePrefix("127.0.0.0/8")}
	}, nil)
	wtTransport := &testWTTransport{Transport: insecureWTTransport(), owner: t}
	id := e.upload.Mint()
	var owner *webtransport.Session
	for _, tc := range []struct{ client, id, want string }{
		{"198.51.100.1", id, "ready"},
		{"198.51.100.2", id, "ownerMismatch"},
		{"198.51.100.1", "gmu_never_minted", "invalid"},
	} {
		sess, status := dialWebTransport(t, wtTransport.Transport, base+"/wt/upload?datagrams=1&id="+tc.id,
			http.Header{"X-Real-IP": {tc.client}})
		if sess == nil {
			t.Fatalf("dial as %s = %d", tc.client, status)
		}
		wtTransport.armClose()
		// A refused session has no status line: its refusal is the one record on its feed.
		if record := nextRecord(t, acceptFeed(t, sess)); cmp.Or(record.Code, record.Type) != tc.want {
			t.Fatalf("%s joining %s: first record %+v, want %q", tc.client, tc.id, record, tc.want)
		}
		owner = cmp.Or(owner, sess)
	}
	// The refused peers never close, and their sessions still give their slots back well inside the idle bound.
	_ = owner.CloseWithError(0, "")
	testkit.Eventually(t, 10*time.Second, "refused sessions give their slots back", func() bool {
		requests, _ := e.admission.stats()
		return requests.active == 0
	})
}

// runGoClientUnderLifetimeCaps runs the shipped client across several request and session bounds per stage.
func runGoClientUnderLifetimeCaps(t *testing.T, throughputTransport, latencyTransport string) {
	t.Helper()
	_, httpBase, _ := wtServer(t, func(c *config.Config) {
		c.MaxOperationDuration = 400 * time.Millisecond
		c.MaxSessionDuration = 400 * time.Millisecond
	}, nil)

	clientCfg := goclient.DefaultConfig()
	clientCfg.BaseURL = httpBase
	clientCfg.ThroughputTransport = throughputTransport
	clientCfg.LatencyTransport = latencyTransport
	clientCfg.InsecureSkipTLSVerify = true
	clientCfg.Stages = goclient.StageSet{Latency: true, Download: true, Upload: true}
	clientCfg.Warmup = 100 * time.Millisecond
	clientCfg.LatencyDuration = goclient.StageBound.Min
	clientCfg.DownloadDuration = goclient.StageBound.Min
	clientCfg.UploadDuration = goclient.StageBound.Min

	ctx, cancel := context.WithTimeout(t.Context(), 60*time.Second)
	defer cancel()
	results := map[string]goclient.Result{}
	var details *goclient.RunDetails
	err := goclient.Run(ctx, clientCfg, func(e goclient.Event) {
		collectStageResults(e, results)
		if e.Kind == goclient.EventDone {
			details = e.Servers
		}
	})
	if err != nil {
		t.Fatalf("run under lifetime caps: %v", err)
	}
	if server := details.Servers[0]; server.Throughput.Transport != throughputTransport ||
		server.LatencyTarget.Transport != latencyTransport {
		t.Fatalf("throughput over %q and latency over %q, want %q and %q", server.Throughput.Transport,
			server.LatencyTarget.Transport, throughputTransport, latencyTransport)
	}
	if got := results["latency"].Latency.Count; got == 0 {
		t.Error("latency stage collected no samples across bus reconnects")
	}
	for _, stage := range []string{"download", "upload"} {
		if got := results[stage].TotalBytes; got == 0 {
			t.Errorf("%s stage moved no bytes across reconnects", stage)
		}
	}
}

func collectStageResults(e goclient.Event, results map[string]goclient.Result) {
	if e.Kind == goclient.EventResult {
		results[string(e.Stage)] = *e.Result
	}
	if e.Kind == goclient.EventDone && e.Servers != nil && len(e.Servers.Servers) == 1 {
		for _, result := range e.Servers.Servers[0].Results {
			if result.Stage == goclient.StageLatency {
				results[string(result.Stage)] = result
			}
		}
	}
}

func TestGoClientOutlivesSessionBoundOverWebTransport(t *testing.T) {
	t.Parallel()
	runGoClientUnderLifetimeCaps(t, "webtransport", "webtransport")
}

func TestGoClientOutlivesOperationBoundOverFetch(t *testing.T) {
	t.Parallel()
	runGoClientUnderLifetimeCaps(t, "fetch-stream", "websocket")
}

// closeSessionBudget refuses every later WebTransport session with the 503 a saturated session budget answers.
func closeSessionBudget(a *requestAdmission) {
	a.mu.Lock()
	defer a.mu.Unlock()
	a.sessions.limit = 0
}

// wtClientConfig is the shipped client pointed at a test server over WebTransport.
func wtClientConfig(httpBase string) goclient.Config {
	cfg := goclient.DefaultConfig()
	cfg.BaseURL = httpBase
	cfg.ThroughputTransport = wire.TransportWebTransport
	cfg.InsecureSkipTLSVerify = true
	cfg.LoadedLatency = false
	cfg.Warmup = 100 * time.Millisecond
	return cfg
}

// A session that is refused for the rest of a measured window has to fail the stage.
func TestWebTransportStageFailsWhenTheSessionIsRefusedMidWindow(t *testing.T) {
	t.Parallel()
	_, httpBase, e := wtServer(t, func(c *config.Config) {
		// The session bound kills the stage's session after it measured 800 ms of the window.
		c.MaxOperationDuration = 1500 * time.Millisecond
		c.MaxSessionDuration = 1500 * time.Millisecond
	}, nil)

	clientCfg := wtClientConfig(httpBase)
	clientCfg.Stages = goclient.StageSet{Download: true}
	clientCfg.DownloadDuration = 6 * time.Second

	ctx, cancel := context.WithTimeout(t.Context(), 60*time.Second)
	defer cancel()
	var mu sync.Mutex
	closed := false
	var downloadResults []goclient.Result
	var details *goclient.RunDetails
	err := goclient.Run(ctx, clientCfg, func(ev goclient.Event) {
		mu.Lock()
		defer mu.Unlock()
		switch ev.Kind {
		case goclient.EventServers, goclient.EventDone:
			details = ev.Servers
		case goclient.EventThroughput:
			// Bytes are moving inside the measured window, so the stage's own session is established.
			if !closed {
				closed = true
				closeSessionBudget(e.admission)
			}
		case goclient.EventResult:
			if ev.Stage == "download" {
				downloadResults = append(downloadResults, *ev.Result)
			}
		}
	})

	mu.Lock()
	defer mu.Unlock()
	if !closed {
		t.Fatal("the stage never reported a throughput sample, so the session budget was never shut")
	}
	if len(downloadResults) != 1 {
		t.Fatalf("failed download emitted %d results, want one incomplete receiver window", len(downloadResults))
	}
	// A sole server lost late keeps the rate of the interval it finished; the run goes on and names why.
	result := downloadResults[0]
	if err != nil || result.Err == nil || result.TotalBytes == 0 || result.Unavailable || result.MeanBps <= 0 {
		t.Fatalf("a sole server refused mid-window must keep its finished interval's rate: %+v; run error: %v",
			result, err)
	}
	// Whichever detector wins, the error names the lost session or its stalled lane.
	if !strings.Contains(result.Err.Error(), "webtransport session lost and not replaced within 2s") &&
		!strings.Contains(result.Err.Error(), "stopped delivering bytes") {
		t.Fatalf("stage err = %q, want it to name the unreplaced session or its stall", result.Err)
	}
	if details == nil || details.Outcome != goclient.OutcomePartial || len(details.Failures) != 1 ||
		len(details.Intervals) != 1 || details.Intervals[0].Window == nil || *details.Intervals[0].Window.DownBytesPerSec <= 0 {
		t.Fatalf("earlier receiver window lost: %+v", details)
	}
}

// The shipped client runs each stage's lanes, one to the published maximum, on one WebTransport session.
func TestGoClientRunsMultipleLanesOverWebTransport(t *testing.T) {
	t.Parallel()
	_, httpBase, e := wtServer(t, nil, nil)
	for _, streams := range []int{1, wire.WTMaxStreams} {
		clientCfg := wtClientConfig(httpBase)
		clientCfg.Stages = goclient.StageSet{Download: true, Upload: true}
		clientCfg.DownloadDuration = 300 * time.Millisecond
		clientCfg.UploadDuration = 300 * time.Millisecond
		clientCfg.TransferStreams = goclient.TransferStreamPolicy{Forced: streams}
		ctx, cancel := context.WithTimeout(t.Context(), 60*time.Second)
		results := map[string]goclient.Result{}
		counting := false
		err := goclient.Run(ctx, clientCfg, func(ev goclient.Event) {
			if ev.Kind == goclient.EventStage && ev.Phase == goclient.PhasePreparing && !counting {
				counting = true
				countSessionsFromNow(t, e.admission)
			}
			if ev.Kind == goclient.EventResult && ev.Result != nil {
				results[string(ev.Stage)] = *ev.Result
			}
		})
		cancel()
		if err != nil || results["download"].TotalBytes == 0 || results["upload"].TotalBytes == 0 {
			t.Fatalf("run at %d lanes: %v, %+v", streams, err, results)
		}
		// Lanes share their stage's session; only the previous stage's closing session may briefly overlap it.
		if _, sessions := e.admission.stats(); sessions.peak > 2 {
			t.Fatalf("%d WebTransport sessions were open at once at %d lanes, want at most 2", sessions.peak, streams)
		}
	}
}

// countSessionsFromNow waits out the preparation's verify session, then restarts the session peak.
func countSessionsFromNow(t *testing.T, a *requestAdmission) {
	for deadline := time.Now().Add(5 * time.Second); ; time.Sleep(10 * time.Millisecond) {
		if _, sessions := a.stats(); sessions.active == 0 {
			break
		}
		if time.Now().After(deadline) {
			t.Error("the preparation's sessions never closed")
			return
		}
	}
	a.mu.Lock()
	a.sessions.peak = 0
	a.mu.Unlock()
}

// A reset upload lane costs only its own bytes: the session, its siblings and a replacement lane carry on.
func TestWebTransportLaneResetLeavesTheSessionIntact(t *testing.T) {
	t.Parallel()
	base, httpBase, wtTransport := wtTestServer(t, nil, nil)
	id := mintUploadID(t, httpBase)
	sess := dialWT(t, wtTransport, base+"/wt/upload?id="+id)
	ctx, cancel := context.WithTimeout(t.Context(), 10*time.Second)
	defer cancel()
	feed := acceptFeed(t, sess)
	if record := nextRecord(t, feed); record.Type != "ready" {
		t.Fatalf("first record %+v, want ready", record)
	}
	const chunk = 1 << 20
	write := func(lane *webtransport.SendStream) {
		t.Helper()
		if _, err := io.CopyN(lane, zeroes{}, chunk); err != nil {
			t.Fatalf("write lane: %v", err)
		}
	}
	open := func() *webtransport.SendStream {
		t.Helper()
		lane, err := sess.OpenUniStreamSync(ctx)
		if err != nil {
			t.Fatalf("open lane: %v", err)
		}
		write(lane)
		return lane
	}
	lanes := []*webtransport.SendStream{open(), open(), open(), open()}
	lanes[1].CancelWrite(0)
	lanes = append(slices.Delete(lanes, 1, 2), open())
	for _, lane := range lanes {
		write(lane)
		if err := lane.Close(); err != nil {
			t.Fatalf("close lane: %v", err)
		}
	}
	finish, err := http.NewRequest(http.MethodDelete, httpBase+"/upload/progress?id="+id, nil)
	if err != nil {
		t.Fatal(err)
	}
	res, err := http.DefaultClient.Do(finish)
	if err != nil {
		t.Fatalf("finish upload: %v", err)
	}
	res.Body.Close()
	for {
		switch record := nextRecord(t, feed); record.Type {
		case "":
			t.Fatal("progress stream never reported complete")
		case "complete":
			// Three siblings and the replacement carry two chunks each; the reset lane at most one.
			if record.Bytes < 8*chunk || record.Bytes > 9*chunk || sess.Context().Err() != nil {
				t.Fatalf("complete counted %d bytes, session error %v", record.Bytes, sess.Context().Err())
			}
			return
		}
	}
}

type zeroes struct{}

func (zeroes) Read(p []byte) (int, error) { return len(p), nil }
