package server

import (
	"bufio"
	"context"
	"crypto/tls"
	"encoding/json/v2"
	"errors"
	"io"
	"net/http"
	"net/netip"
	"net/url"
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
	httpBase := "http://" + cfg.Native.H1
	waitForOK(t, http.DefaultClient, httpBase+"/preflight")
	return "https://" + cfg.Native.H3, httpBase, build.e
}

// startListeners runs the listeners tune reserves, under a test certificate, until the test ends.
func startListeners(t *testing.T, tune func(*config.Config, *testListenerSockets),
	shape func(*endpoints)) (*config.Config, *listenerBuild) {
	t.Helper()
	cert, key := writeCertificate(t, t.TempDir(), "srv", "127.0.0.1",
		time.Now().Add(-time.Hour), time.Now().Add(time.Hour))
	sockets := newTestListenerSockets(t)
	cfg := config.Default()
	cfg.TLSCert, cfg.TLSKey = cert, key
	tune(&cfg, sockets)
	ctx, cancel := context.WithCancel(t.Context())
	t.Cleanup(cancel)
	build, err := newListenerBuild(ctx, &cfg, sockets)
	if err != nil {
		t.Fatalf("build listeners: %v", err)
	}
	if shape != nil {
		shape(build.e)
	}
	if err := build.assemble(); err != nil {
		t.Fatalf("assemble listeners: %v", err)
	}
	for _, svc := range build.services {
		run, stop := svc.run, svc.stop
		go func() { _ = run() }()
		// Service cleanup must still run after t.Context is canceled.
		t.Cleanup(func() { _ = stop(context.Background()) })
	}
	return &cfg, build
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

// dialWT opens a session, retrying while the QUIC listener finishes coming up.
func dialWT(t *testing.T, wtTransport *testWTTransport, url string) *webtransport.Session {
	t.Helper()
	ctx, cancel := context.WithTimeout(t.Context(), 10*time.Second)
	defer cancel()
	for {
		_, sess, err := wtTransport.Dial(ctx, url, nil)
		if err == nil {
			wtTransport.armClose()
			t.Cleanup(func() { _ = sess.CloseWithError(0, "") })
			return sess
		}
		if ctx.Err() != nil {
			t.Fatalf("dial %s: %v", url, err)
		}
		time.Sleep(20 * time.Millisecond)
	}
}

func TestWebTransportPingEchoesOverDatagrams(t *testing.T) {
	t.Parallel()
	base, _, wtTransport := wtTestServer(t, nil, nil)
	sess := dialWT(t, wtTransport, base+"/wt/ping")

	ctx, cancel := context.WithTimeout(t.Context(), 10*time.Second)
	defer cancel()

	// Datagrams may be dropped, so each frame is re-sent until its reply lands or the window closes.
	if !echoes(t, ctx, sess, 42, func(reply string) bool {
		f, err := wire.DecodePong(reply)
		return err == nil && f.ID == 42
	}) {
		t.Fatal("PING never drew PONG with id 42")
	}
}

// echoes sends frame until want accepts a reply or ctx ends.
func echoes(t *testing.T, ctx context.Context, sess *webtransport.Session, id uint32, want func(string) bool) bool {
	t.Helper()
	for ctx.Err() == nil {
		if err := sess.SendDatagram([]byte(wire.EncodePing(id))); err != nil {
			t.Fatalf("send PING,%d: %v", id, err)
		}
		replyCtx, cancel := context.WithTimeout(ctx, 500*time.Millisecond)
		reply, err := sess.ReceiveDatagram(replyCtx)
		cancel()
		if err == nil && want(string(reply)) {
			return true
		}
	}
	return false
}

func TestWebTransportDownloadServesTheRequestedSize(t *testing.T) {
	t.Parallel()
	base, _, wtTransport := wtTestServer(t, nil, nil)
	sess := dialWT(t, wtTransport, base+"/wt/download?bytes=1048576&streams=2")

	ctx, cancel := context.WithTimeout(t.Context(), 10*time.Second)
	defer cancel()
	const want = 1 << 20
	for lane := range 3 {
		str, err := sess.AcceptUniStream(ctx)
		if err != nil {
			t.Fatalf("accept lane %d: %v", lane, err)
		}
		n, err := io.Copy(io.Discard, str)
		if err != nil {
			t.Fatalf("read lane %d: %v", lane, err)
		}
		if n != want {
			t.Fatalf("lane %d served %d bytes, want %d", lane, n, want)
		}
	}
}

// Every clamped lane delivers, and none past the cap opens once they all have.
func TestWebTransportDownloadClampsTheLaneCount(t *testing.T) {
	t.Parallel()
	base, _, wtTransport := wtTestServer(t, nil, nil)
	sess := dialWT(t, wtTransport, base+"/wt/download?bytes=67108864&streams=99")
	ctx, cancel := context.WithTimeout(t.Context(), 30*time.Second)
	defer cancel()
	for lane := range wire.WTMaxStreams {
		str, err := sess.AcceptUniStream(ctx)
		if err != nil {
			t.Fatalf("lane %d: %v", lane, err)
		}
		if _, err := io.ReadFull(str, make([]byte, 1)); err != nil {
			t.Fatalf("lane %d delivered nothing: %v", lane, err)
		}
	}
	extra, cancelExtra := context.WithTimeout(ctx, time.Second)
	defer cancelExtra()
	if _, err := sess.AcceptUniStream(extra); err == nil {
		t.Fatalf("a lane past the %d-lane cap opened", wire.WTMaxStreams)
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

func TestWebTransportUploadClampsTheLaneCount(t *testing.T) {
	t.Parallel()
	base, httpBase, wtTransport := wtTestServer(t, nil, nil)
	sess := dialWT(t, wtTransport, base+"/wt/upload?id="+mintUploadID(t, httpBase))

	// Every lane writes for the same window, paced so the test moves tens of megabytes rather than everything the link.
	const opened = wire.WTMaxStreams + 4
	const laneWindow = 500 * time.Millisecond
	block := make([]byte, 16<<10)
	var writable atomic.Int64
	var wg sync.WaitGroup
	for lane := range opened {
		openCtx, cancelOpen := context.WithTimeout(t.Context(), 10*time.Second)
		str, err := sess.OpenUniStreamSync(openCtx)
		cancelOpen()
		if err != nil {
			t.Fatalf("open lane %d: %v", lane, err)
		}
		wg.Go(func() {
			// Each lane times its own window from where it starts writing.
			deadline := time.Now().Add(laneWindow)
			// A lane neither drained nor reset would park this test on flow control.
			if err := str.SetWriteDeadline(deadline.Add(5 * time.Second)); err != nil {
				t.Errorf("lane %d write deadline: %v", lane, err)
				return
			}
			for time.Now().Before(deadline) {
				if _, err := str.Write(block); err != nil {
					return
				}
				time.Sleep(10 * time.Millisecond)
			}
			writable.Add(1)
		})
	}
	wg.Wait()
	if got := writable.Load(); got != wire.WTMaxStreams {
		t.Fatalf("%d of %d lanes stayed writable, want the %d cap with the excess reset", got, opened,
			wire.WTMaxStreams)
	}
}

func TestWebTransportUploadDrainsDatagrams(t *testing.T) {
	t.Parallel()
	base, httpBase, wtTransport := wtTestServer(t, nil, nil)
	sess := dialWT(t, wtTransport, base+"/wt/upload?datagrams=1&id="+mintUploadID(t, httpBase))
	acceptCtx, cancelAccept := context.WithTimeout(t.Context(), 10*time.Second)
	defer cancelAccept()
	progress, err := sess.AcceptUniStream(acceptCtx)
	if err != nil {
		t.Fatalf("accept progress stream: %v", err)
	}
	records := bufio.NewScanner(progress)
	if !firstProgressTypeIs(t, records, "ready") {
		t.Fatal("progress stream never reported ready")
	}

	// Datagrams are lossy, so the assertion is that the drain counts them, not that every one lands.
	payload := make([]byte, 1000)
	done := make(chan struct{})
	go func() {
		defer close(done)
		for {
			select {
			case <-done:
				return
			default:
			}
			if err := sess.SendDatagram(payload); err != nil {
				return
			}
			time.Sleep(2 * time.Millisecond)
		}
	}()
	defer sess.CloseWithError(0, "") //nolint:errcheck // the test is ending either way

	deadline := time.Now().Add(10 * time.Second)
	for time.Now().Before(deadline) {
		if !records.Scan() {
			break
		}
		if strings.TrimSpace(records.Text()) == "" {
			continue
		}
		var event struct {
			Type  string `json:"type"`
			Bytes uint64 `json:"bytes"`
		}
		if json.Unmarshal(records.Bytes(), &event) != nil {
			continue
		}
		if event.Type == "progress" && event.Bytes > 0 {
			return
		}
	}
	t.Fatal("datagram upload never reached the server-authoritative counter")
}

// A refused upload session has no status line: its refusal is the one record on its feed, and the session then ends.
func TestWebTransportUploadReportsARefusedIDAndFreesItsSlot(t *testing.T) {
	t.Parallel()
	base, httpBase, wtTransport := wtTestServer(t, nil, nil)
	sess := dialWT(t, wtTransport, base+"/wt/upload?datagrams=1&id=gmu_never_minted")
	defer sess.CloseWithError(0, "") //nolint:errcheck // the test is ending either way
	acceptCtx, cancelAccept := context.WithTimeout(t.Context(), 10*time.Second)
	defer cancelAccept()
	progress, err := sess.AcceptUniStream(acceptCtx)
	if err != nil {
		t.Fatalf("accept progress stream: %v", err)
	}
	records := bufio.NewScanner(progress)
	for records.Scan() {
		line := strings.TrimSpace(records.Text())
		if line == "" {
			continue
		}
		var event struct {
			Type    string `json:"type"`
			Message string `json:"message"`
		}
		if err := json.Unmarshal([]byte(line), &event); err != nil {
			t.Fatalf("decode record %q: %v", line, err)
		}
		if event.Type != "error" {
			t.Fatalf("first record = %q, want the refusal", line)
		}
		if event.Message != "unknown upload id" {
			t.Fatalf("refusal message = %q", event.Message)
		}
		// The peer never closes, and the refused session still gives its slot back well inside the idle bound.
		waitForLoad(t, httpBase, 0)
		return
	}
	t.Fatal("a refused upload reported nothing")
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

	ctx, cancel := context.WithTimeout(t.Context(), time.Second)
	defer cancel()
	if str, err := sess.AcceptUniStream(ctx); err == nil {
		t.Fatalf("verify session opened a stream: %v", str)
	}
	// A second's worth of accepting has passed, so a session that was going to be torn down has been.
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
	} {
		t.Run(tc.reason, func(t *testing.T) {
			t.Parallel()
			base, _, wtTransport := wtTestServer(t, tc.tune, tc.shape)
			sess := dialWT(t, wtTransport, base+"/wt/ping")
			ctx, cancel := context.WithTimeout(t.Context(), 10*time.Second)
			defer cancel()
			_, err := sess.AcceptUniStream(ctx)
			closed, ok := errors.AsType[*webtransport.SessionError](err)
			if !ok || closed.ErrorCode != tc.code || closed.Message != tc.reason {
				t.Fatalf("session ended with %v, want %d %q", err, tc.code, tc.reason)
			}
		})
	}
}

// A stream download's liveness is the peer draining its lanes, and that is the only thing keeping the session open.
func TestDrainedStreamDownloadOutlivesTheIdleBound(t *testing.T) {
	t.Parallel()
	const bound = 300 * time.Millisecond
	base, _, wtTransport := wtTestServer(t, nil, idleBound(bound))
	sess := dialWT(t, wtTransport, base+"/wt/download?bytes=262144&streams=1")

	ctx, cancel := context.WithTimeout(t.Context(), 30*time.Second)
	defer cancel()
	start := time.Now()
	// Reaping takes at most 1.5 bounds, so surviving four proves draining is what kept it.
	deadline := start.Add(4 * bound)
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
	for status, err := probe(1024); status != http.StatusOK; status, err = probe(1024) {
		if ctx.Err() != nil {
			t.Fatalf("ordinary request = %d, %v", status, err)
		}
		time.Sleep(20 * time.Millisecond)
	}
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
			base, httpBase, tr := wtTestServer(t, nil, idleBound(300*time.Millisecond))
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
	feed, err := sess.AcceptUniStream(t.Context())
	if err != nil || !firstProgressTypeIs(t, bufio.NewScanner(feed), "ready") {
		t.Fatalf("progress feed: %v", err)
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
	acceptCtx, cancelAccept := context.WithTimeout(t.Context(), 10*time.Second)
	defer cancelAccept()
	refusal, err := sess.AcceptUniStream(acceptCtx)
	if err != nil || !firstProgressTypeIs(t, bufio.NewScanner(refusal), "error") {
		t.Fatalf("refused lane reported no refusal record: %v", err)
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
	deadline := time.Now().Add(10 * time.Second)
	for {
		if got := probeLoad(t, httpBase); got == want {
			return
		} else if time.Now().After(deadline) {
			t.Fatalf("occupancy stayed at %d, want %d", got, want)
		}
		time.Sleep(20 * time.Millisecond)
	}
}

// Enforce is the only origin policy a WebTransport CONNECT passes through.
func TestWebTransportConnectRefusesAForeignOrigin(t *testing.T) {
	t.Parallel()
	s := newAuthenticatedStack(t)
	for _, path := range []string{route.WTPing, route.WTDownload, route.WTUpload} {
		foreign := http.Header{"Origin": {"https://attacker.example"}}
		target := func() string { return s.h3URL + path + "?token=" + url.QueryEscape(s.mintWTTokenFor(t, path)) }
		res, sess, err := dialWTUntilAnswered(t, s.wtTransport(t), target(), foreign)
		if err == nil {
			_ = sess.CloseWithError(0, "")
			t.Fatalf("a %s CONNECT carrying a foreign Origin opened a session", path)
		}
		if res == nil {
			t.Fatalf("foreign-Origin %s CONNECT failed without a response: %v", path, err)
		}
		if res.StatusCode == http.StatusOK {
			t.Fatalf("foreign-Origin %s CONNECT status=%d, want a refusal", path, res.StatusCode)
		}

		// The control: the same credential, transport and header, and only the origin canonical.
		res, sess, err = dialWTUntilAnswered(t, s.wtTransport(t), target(), http.Header{"Origin": {s.origin}})
		if err != nil {
			t.Fatalf("%s CONNECT from the canonical origin was refused with status=%v: %v", path, res, err)
		}
		_ = sess.CloseWithError(0, "")
	}
}

// An upload session joins only its own client's receiver, whoever holds the id.
func TestWebTransportUploadRefusesAnotherClientsReceiver(t *testing.T) {
	t.Parallel()
	loopback := netip.MustParsePrefix("127.0.0.0/8")
	base, httpBase, wtTransport := wtTestServer(t, func(c *config.Config) {
		c.TrustedProxies = []netip.Prefix{loopback}
	}, nil)
	id := mintUploadID(t, httpBase)
	firstRecord := func(client, want string) {
		t.Helper()
		res, sess, err := dialWTUntilAnswered(t, wtTransport.Transport, base+"/wt/upload?id="+id,
			http.Header{"X-Real-IP": {client}})
		if err != nil {
			t.Fatalf("dial as %s: %v %v", client, res, err)
		}
		wtTransport.armClose()
		t.Cleanup(func() { _ = sess.CloseWithError(0, "") })
		ctx, cancel := context.WithTimeout(t.Context(), 10*time.Second)
		defer cancel()
		feed, err := sess.AcceptUniStream(ctx)
		if err != nil || !firstProgressTypeIs(t, bufio.NewScanner(feed), want) {
			t.Fatalf("%s's first record is not %q: %v", client, want, err)
		}
	}
	firstRecord("198.51.100.1", "ready")
	firstRecord("198.51.100.2", "error")
}

// dialWTUntilAnswered dials until the listener answers, so a QUIC listener still coming up is not read as a refusal.
func dialWTUntilAnswered(t *testing.T, d *webtransport.Transport, target string, hdr http.Header) (*http.Response,
	*webtransport.Session, error) {
	t.Helper()
	ctx, cancel := context.WithTimeout(t.Context(), 10*time.Second)
	defer cancel()
	for {
		res, sess, err := d.Dial(ctx, target, hdr)
		if err == nil || res != nil {
			return res, sess, err
		}
		if ctx.Err() != nil {
			t.Fatalf("dial %s: %v", target, err)
		}
		time.Sleep(20 * time.Millisecond)
	}
}

// A lane that keeps dropping redials, and the budget it needs is one however many times it does.
func TestFlappingWebTransportLaneCostsOneSlot(t *testing.T) {
	t.Parallel()
	base, httpBase, wtTransport := wtTestServer(t, func(c *config.Config) {
		c.MaxSessionsPerClient = 1
	}, idleBound(300*time.Millisecond))

	for attempt := 1; attempt <= 3; attempt++ {
		dialWT(t, wtTransport, base+"/wt/download?bytes=1073741824&streams=1")
		waitForLoad(t, httpBase, 1)
		waitForLoad(t, httpBase, 0)
	}
}

func TestWebTransportUploadCountsLanesOnItsProgressStream(t *testing.T) {
	t.Parallel()
	base, httpBase, wtTransport := wtTestServer(t, nil, nil)

	// The id is minted and finalized over HTTP; only the bytes ride the session.
	client := http.DefaultClient
	id := mintUploadID(t, httpBase)
	sess := dialWT(t, wtTransport, base+"/wt/upload?id="+id)
	acceptCtx, cancelAccept := context.WithTimeout(t.Context(), 10*time.Second)
	defer cancelAccept()
	progress, err := sess.AcceptUniStream(acceptCtx)
	if err != nil {
		t.Fatalf("accept progress stream: %v", err)
	}
	records := bufio.NewScanner(progress)
	if !firstProgressTypeIs(t, records, "ready") {
		t.Fatal("progress stream never reported ready")
	}

	const want = 4 << 20
	lane, err := sess.OpenUniStreamSync(t.Context())
	if err != nil {
		t.Fatalf("open lane: %v", err)
	}
	if _, err := io.CopyN(lane, zeroes{}, want); err != nil {
		t.Fatalf("write lane: %v", err)
	}
	if err := lane.Close(); err != nil {
		t.Fatalf("close lane: %v", err)
	}

	req, err := http.NewRequest(http.MethodDelete, httpBase+"/upload/progress?id="+id, nil)
	if err != nil {
		t.Fatal(err)
	}
	finish, err := client.Do(req)
	if err != nil {
		t.Fatalf("finish upload: %v", err)
	}
	finish.Body.Close()

	for records.Scan() {
		if strings.TrimSpace(records.Text()) == "" {
			continue
		}
		var event struct {
			Type  string `json:"type"`
			Bytes uint64 `json:"bytes"`
		}
		if err := json.Unmarshal(records.Bytes(), &event); err != nil {
			t.Fatalf("decode record %q: %v", records.Text(), err)
		}
		if event.Type != "complete" {
			continue
		}
		if event.Bytes != want {
			t.Fatalf("complete counted %d bytes, want %d", event.Bytes, want)
		}
		return
	}
	t.Fatal("progress stream never reported complete")
}

func TestGoClientRunsOverWebTransport(t *testing.T) {
	t.Parallel()
	_, httpBase, _ := wtServer(t, nil, nil)

	clientCfg := goclient.DefaultConfig()
	clientCfg.BaseURL = httpBase
	clientCfg.ThroughputTransport = "webtransport"
	clientCfg.InsecureSkipTLSVerify = true
	clientCfg.Stages = goclient.StageSet{Latency: true, Download: true, Upload: true}
	clientCfg.Warmup = 100 * time.Millisecond
	clientCfg.LatencyDuration = 300 * time.Millisecond
	clientCfg.DownloadDuration = 500 * time.Millisecond
	clientCfg.UploadDuration = 500 * time.Millisecond

	ctx, cancel := context.WithTimeout(t.Context(), 30*time.Second)
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
		t.Fatalf("run: %v", err)
	}
	if got := details.Servers[0].Throughput.Transport; got != wire.TransportWebTransport {
		t.Fatalf("throughput transport = %q, want webtransport", got)
	}
	if got := details.Servers[0].LatencyTarget.Transport; got != wire.TransportWebTransport {
		t.Fatalf("latency transport = %q, want webtransport", got)
	}
	if got := results["latency"].Latency.Count; got == 0 {
		t.Error("latency stage collected no samples over datagrams")
	}
	for _, stage := range []string{"download", "upload"} {
		if got := results[stage].TotalBytes; got == 0 {
			t.Errorf("%s stage moved no bytes", stage)
		}
	}
}

// runGoClientUnderLifetimeCaps runs the shipped client against a server whose request and session bounds are far.
func runGoClientUnderLifetimeCaps(t *testing.T, throughputTransport, latencyTransport string) {
	t.Helper()
	_, httpBase, _ := wtServer(t, func(c *config.Config) {
		c.MaxOperationDuration = 750 * time.Millisecond
		c.MaxSessionDuration = 750 * time.Millisecond
	}, nil)

	clientCfg := goclient.DefaultConfig()
	clientCfg.BaseURL = httpBase
	clientCfg.ThroughputTransport = throughputTransport
	clientCfg.LatencyTransport = latencyTransport
	clientCfg.InsecureSkipTLSVerify = true
	clientCfg.Stages = goclient.StageSet{Latency: true, Download: true, Upload: true}
	clientCfg.Warmup = 100 * time.Millisecond
	clientCfg.LatencyDuration = 2 * time.Second
	clientCfg.DownloadDuration = 2 * time.Second
	clientCfg.UploadDuration = 2 * time.Second

	ctx, cancel := context.WithTimeout(t.Context(), 60*time.Second)
	defer cancel()
	results := map[string]goclient.Result{}
	err := goclient.Run(ctx, clientCfg, func(e goclient.Event) { collectStageResults(e, results) })
	if err != nil {
		t.Fatalf("run under lifetime caps: %v", err)
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
		// The session bound kills the stage's session early in the window.
		c.MaxOperationDuration = time.Second
		c.MaxSessionDuration = time.Second
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
	if err == nil {
		t.Fatal("a download refused for the rest of its window returned no error: the shortfall became a rate")
	}
	// Whichever detector wins, the error names the lost session or its stalled lane.
	if !strings.Contains(err.Error(), "webtransport session lost and not replaced within 2s") &&
		!strings.Contains(err.Error(), "stopped delivering bytes") {
		t.Fatalf("stage err = %q, want it to name the unreplaced session or its stall", err)
	}
	if len(downloadResults) != 1 {
		t.Fatalf("failed download emitted %d results, want one incomplete receiver window", len(downloadResults))
	}
	result := downloadResults[0]
	if result.Err != err || result.TotalBytes == 0 || !result.Unavailable || result.ReceiverTimed() {
		t.Fatalf("all servers failing must retain bytes and error with an unavailable headline: %+v; run error: %v",
			result, err)
	}
	if details == nil || len(details.Failures) != 1 || len(details.Intervals) < 2 ||
		details.Intervals[0].Window == nil || *details.Intervals[0].Window.DownBytesPerSec <= 0 {
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
		clientCfg.DownloadDuration = 600 * time.Millisecond
		clientCfg.UploadDuration = 600 * time.Millisecond
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
	progress, err := sess.AcceptUniStream(ctx)
	if err != nil {
		t.Fatalf("accept progress stream: %v", err)
	}
	records := bufio.NewScanner(progress)
	if !firstProgressTypeIs(t, records, "ready") {
		t.Fatal("progress stream never reported ready")
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
	for records.Scan() {
		event, err := wire.DecodeUploadProgress(records.Bytes())
		if err != nil || event.Type != "complete" {
			continue
		}
		// Three siblings and the replacement carry two chunks each; the reset lane at most one.
		if event.Bytes < 8*chunk || event.Bytes > 9*chunk || sess.Context().Err() != nil {
			t.Fatalf("complete counted %d bytes, session error %v", event.Bytes, sess.Context().Err())
		}
		return
	}
	t.Fatal("progress stream never reported complete")
}

// firstProgressTypeIs reports whether the FIRST non-blank record has this type.
func firstProgressTypeIs(t *testing.T, records *bufio.Scanner, want string) bool {
	t.Helper()
	for records.Scan() {
		line := strings.TrimSpace(records.Text())
		if line == "" {
			continue
		}
		var event struct {
			Type string `json:"type"`
		}
		if err := json.Unmarshal([]byte(line), &event); err != nil {
			t.Fatalf("decode record %q: %v", line, err)
		}
		return event.Type == want
	}
	return false
}

type zeroes struct{}

func (zeroes) Read(p []byte) (int, error) { return len(p), nil }
