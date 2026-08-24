package server

import (
	"bufio"
	"context"
	"crypto/tls"
	"encoding/json"
	"errors"
	"io"
	"net/http"
	"net/url"
	"os"
	"strconv"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"github.com/quic-go/webtransport-go"
	"github.com/zR-JB/graphite-meter/go/internal/config"
	"github.com/zR-JB/graphite-meter/go/internal/endpoint"
	"github.com/zR-JB/graphite-meter/go/internal/goclient"
	"github.com/zR-JB/graphite-meter/go/internal/transport"
	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

// wtTestServer boots a server with HTTP/3 and returns the WebTransport base URL,
// the clear-H1 base URL for the HTTP half of an upload, and a session transport.
func wtTestServer(t *testing.T) (string, string, *testWTTransport) {
	t.Helper()
	return wtTestServerTuned(t, nil)
}

// wtTestServerTuned is wtTestServer with the config open to the caller.
func wtTestServerTuned(t *testing.T, tune func(*config.Config)) (string, string, *testWTTransport) {
	t.Helper()
	h3Base, httpBase := wtTestOrigins(t, tune)
	return h3Base, httpBase, &testWTTransport{Transport: insecureWTTransport(), owner: t}
}

// testWTTransport is a session transport whose close is armed only once a dial has
// landed. webtransport.Transport.Close cancels a context only the first Dial
// installs, so closing one that never dialed dereferences nil -- and a panic in
// t.Cleanup aborts the whole test binary rather than failing one test, which
// under -shuffle=on leaves a different surviving set per seed.
type testWTTransport struct {
	*webtransport.Transport
	owner *testing.T
	arm   sync.Once
}

// armClose ties the transport to the test that asked for it, not to whichever
// subtest happened to dial first: several subtests share one transport.
func (d *testWTTransport) armClose() {
	d.arm.Do(func() { d.owner.Cleanup(func() { _ = d.Transport.Close() }) })
}

// wtTestOrigins boots the same server for a test that drives it through the
// shipped client rather than a raw session: the HTTP/3 origin and the clear-H1
// one, no transport.
func wtTestOrigins(t *testing.T, tune func(*config.Config)) (string, string) {
	t.Helper()
	cfg, sockets := wtTestConfig(t, tune)
	t.Cleanup(runUntilCancel(t, &cfg, sockets))
	waitForOK(t, http.DefaultClient, "http://"+cfg.Native.H1+"/preflight")
	return "https://" + cfg.Native.H3, "http://" + cfg.Native.H1
}

func wtTestConfig(t *testing.T, tune func(*config.Config)) (config.Config, *testListenerSockets) {
	t.Helper()
	cert, key := writeCertificate(t, t.TempDir(), "srv", "127.0.0.1",
		time.Now().Add(-time.Hour), time.Now().Add(time.Hour))
	sockets := newTestListenerSockets(t)
	cfg := config.Default()
	cfg.Native.H1 = sockets.reserveTCP()
	cfg.Native.H3 = sockets.reserveH3()
	cfg.TLSCert, cfg.TLSKey = cert, key
	if tune != nil {
		tune(&cfg)
	}
	return cfg, sockets
}

// wtTestServerWithIdleBound is wtTestServerTuned for the tests that turn on the
// WebTransport idle bound, which is 30 seconds in production and cannot be
// waited out per test. The bound is a constructor argument to each session
// handler, so choosing it means building the endpoints and then assembling the
// listeners around them -- exactly what Run does, through the same two calls,
// with the one value set in between. Nothing test-only ships in the handlers.
func wtTestServerWithIdleBound(t *testing.T, bound time.Duration, tune func(*config.Config)) (string, string, *testWTTransport) {
	t.Helper()
	h3Base, httpBase, _ := wtShapedServer(t, tune, func(e *endpoints) { e.wtIdleBound = bound })
	return h3Base, httpBase, &testWTTransport{Transport: insecureWTTransport(), owner: t}
}

// wtShapedServer is the seam described above, generalized: shape runs on the
// built endpoints between the two calls Run makes, and the endpoints come back
// so a test can watch what the server observed or move a limit mid-run. Nothing
// test-only ships in the handlers.
func wtShapedServer(t *testing.T, tune func(*config.Config), shape func(*endpoints)) (string, string, *endpoints) {
	t.Helper()
	cfg, sockets := wtTestConfig(t, tune)
	ctx, cancel := context.WithCancel(context.Background())
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
		t.Cleanup(func() { _ = stop(context.Background()) })
	}

	httpBase := "http://" + cfg.Native.H1
	waitForOK(t, http.DefaultClient, httpBase+"/preflight")
	return "https://" + cfg.Native.H3, httpBase, build.e
}

// insecureWTTransport dials a test listener's self-signed certificate. The caller
// owns the wtTransport: most tie it to t, the stress harness to a goroutine.
func insecureWTTransport() *webtransport.Transport {
	return &webtransport.Transport{
		TLSClientConfig: &tls.Config{InsecureSkipVerify: true}, //nolint:gosec // self-signed test certificate
		QUICConfig:      transport.NewQUICConfig(),
	}
}

// dialWT opens a session, retrying while the QUIC listener finishes coming up.
func dialWT(t *testing.T, wtTransport *testWTTransport, url string) *webtransport.Session {
	t.Helper()
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
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

// TestWebTransportPingEchoesOverDatagrams drives the latency bus on its
// unreliable channel: the same wire protocol the WebSocket bus speaks.
func TestWebTransportPingEchoesOverDatagrams(t *testing.T) {
	base, _, wtTransport := wtTestServer(t)
	sess := dialWT(t, wtTransport, base+"/wt/ping")

	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()

	// Datagrams may be dropped, so each frame is re-sent until its reply lands
	// or the window closes: what is pinned is the echo, not delivery.
	if !echoes(t, ctx, sess, wire.Frame{Op: wire.OpHI, Proto: "wt"}, func(reply string) bool {
		return reply == wire.OpREADY
	}) {
		t.Fatal("HI never drew READY")
	}
	if !echoes(t, ctx, sess, wire.Frame{Op: wire.OpPING, ID: 42}, func(reply string) bool {
		f, err := wire.Decode(reply)
		return err == nil && f.Op == wire.OpPONG && f.ID == 42
	}) {
		t.Fatal("PING never drew PONG with id 42")
	}
}

// echoes sends frame until want accepts a reply or ctx ends.
func echoes(t *testing.T, ctx context.Context, sess *webtransport.Session, frame wire.Frame, want func(string) bool) bool {
	t.Helper()
	for ctx.Err() == nil {
		if err := sess.SendDatagram([]byte(wire.Encode(frame))); err != nil {
			t.Fatalf("send %s: %v", frame.Op, err)
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

// TestWebTransportDownloadServesTheRequestedSize proves the CONNECT query sizes
// the server-opened lanes, and that an exhausted lane is replaced.
func TestWebTransportDownloadServesTheRequestedSize(t *testing.T) {
	base, _, wtTransport := wtTestServer(t)
	sess := dialWT(t, wtTransport, base+"/wt/download?bytes=1048576&streams=2")

	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
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

// TestWebTransportDownloadClampsTheLaneCount pins the published range: a count
// above the cap yields the cap, and an absent or unusable one yields a single
// lane. Lanes are sized far past what the window drains and left unread, so
// what is counted is concurrent lanes rather than their replacements.
func TestWebTransportDownloadClampsTheLaneCount(t *testing.T) {
	base, _, wtTransport := wtTestServer(t)
	const unread = "bytes=67108864"
	for _, tc := range []struct {
		query string
		want  int
	}{
		{unread + "&streams=99", 16},
		{unread, 1},
		{unread + "&streams=0", 1},
		{unread + "&streams=nonsense", 1},
	} {
		t.Run(tc.query, func(t *testing.T) {
			sess := dialWT(t, wtTransport, base+"/wt/download?"+tc.query)
			ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
			defer cancel()
			// One accept past the expected count must find nothing.
			seen := 0
			for seen <= tc.want {
				accept, cancelAccept := context.WithTimeout(ctx, 500*time.Millisecond)
				_, err := sess.AcceptUniStream(accept)
				cancelAccept()
				if err != nil {
					break
				}
				seen++
			}
			if seen != tc.want {
				t.Errorf("%s opened %d concurrent lanes, want %d", tc.query, seen, tc.want)
			}
		})
	}
}

// mintUploadID mints an upload id over HTTP, the half of an upload that never
// rides the session.
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
	if err := json.NewDecoder(res.Body).Decode(&minted); err != nil {
		t.Fatalf("decode upload session: %v", err)
	}
	if minted.UploadID == "" {
		t.Fatal("upload session returned an empty id")
	}
	return minted.UploadID
}

// TestWebTransportUploadClampsTheLaneCount is the upload direction of the same
// published ceiling the download side clamps to. Client-opened lanes past it are
// reset rather than drained: each drained one holds a goroutine and a 256 KiB
// scratch buffer for the length of the session.
func TestWebTransportUploadClampsTheLaneCount(t *testing.T) {
	base, httpBase, wtTransport := wtTestServer(t)
	sess := dialWT(t, wtTransport, base+"/wt/upload?id="+mintUploadID(t, httpBase))

	// Every lane writes for the same window, paced so the test moves tens of
	// megabytes rather than everything the link will take. A drained lane keeps
	// accepting bytes for the whole window; a reset one fails its next write.
	// The lanes open one at a time because a stream reaches the server only when
	// it is written to: the excess ones are reset as the loop runs, which is what
	// returns the credit the next open needs.
	const opened = wire.WTMaxStreams + 4
	const laneWindow = 2 * time.Second
	block := make([]byte, 16<<10)
	var writable atomic.Int64
	var wg sync.WaitGroup
	for lane := range opened {
		openCtx, cancelOpen := context.WithTimeout(context.Background(), 10*time.Second)
		str, err := sess.OpenUniStreamSync(openCtx)
		cancelOpen()
		if err != nil {
			t.Fatalf("open lane %d: %v", lane, err)
		}
		wg.Go(func() {
			// Each lane times its own window from where it starts writing. One
			// window opened before the loop is spent by however long the opening
			// takes, and a slow enough scheduler would leave every goroutine to
			// skip its body and count as writable -- a failure naming the lane cap
			// rather than the harness.
			deadline := time.Now().Add(laneWindow)
			// A lane neither drained nor reset would park this test on flow
			// control; the write deadline turns that into a failure instead.
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
		t.Fatalf("%d of %d lanes stayed writable, want the %d cap with the excess reset", got, opened, wire.WTMaxStreams)
	}
}

// TestWebTransportUploadDrainsDatagrams covers the datagram upload mode end to
// end: the server counts what arrives and reports it on the progress feed the
// same session carries.
func TestWebTransportUploadDrainsDatagrams(t *testing.T) {
	base, httpBase, wtTransport := wtTestServer(t)
	sess := dialWT(t, wtTransport, base+"/wt/upload?datagrams=1&id="+mintUploadID(t, httpBase))
	acceptCtx, cancelAccept := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancelAccept()
	progress, err := sess.AcceptUniStream(acceptCtx)
	if err != nil {
		t.Fatalf("accept progress stream: %v", err)
	}
	records := bufio.NewScanner(progress)
	if !firstProgressTypeIs(t, records, "ready") {
		t.Fatal("progress stream never reported ready")
	}

	// Datagrams are lossy, so the assertion is that the drain counts them, not
	// that every one lands: send until the feed reports bytes.
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

// A refused datagram drain has no stream to reset and no status line, so it
// reports nothing itself. The progress feed the same session opens is what
// carries the refusal, and both read the id under the same owner, so they
// cannot disagree about it.
func TestWebTransportDatagramUploadReportsARefusedID(t *testing.T) {
	base, _, wtTransport := wtTestServer(t)
	sess := dialWT(t, wtTransport, base+"/wt/upload?datagrams=1&id=gmu_never_minted")
	defer sess.CloseWithError(0, "") //nolint:errcheck // the test is ending either way
	acceptCtx, cancelAccept := context.WithTimeout(context.Background(), 10*time.Second)
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
		var event struct{ Type, Message string }
		if err := json.Unmarshal([]byte(line), &event); err != nil {
			t.Fatalf("decode record %q: %v", line, err)
		}
		if event.Type != "error" {
			t.Fatalf("first record = %q, want the refusal", line)
		}
		if event.Message != "unknown upload id" {
			t.Fatalf("refusal message = %q", event.Message)
		}
		return
	}
	t.Fatal("a refused datagram upload reported nothing")
}

// TestWebTransportDatagramFloodRepeats proves the flood re-runs while the
// session lives: more than one `bytes=` total arrives.
func TestWebTransportDatagramFloodRepeats(t *testing.T) {
	base, _, wtTransport := wtTestServer(t)
	sess := dialWT(t, wtTransport, base+"/wt/download?bytes=2000&datagrams=1")

	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	for got := 0; got <= 2000; {
		d, err := sess.ReceiveDatagram(ctx)
		if err != nil {
			t.Fatalf("flood ended after %d bytes: %v", got, err)
		}
		got += len(d)
	}
}

// TestWebTransportVerifySessionLingersAndServesNothing pins the bytes=0
// transport check, which is two properties and not one: nothing is served, and
// the session lingers rather than closing at once, because its answer is the
// handshake and the client is what closes it.
//
// Asserting only "no stream arrives" held in a world with no park at all: the
// session is torn down before webtransport-go's asynchronous header write can
// deliver the empty lane, so the accept fails either way. Each half is pinned
// on its own here.
func TestWebTransportVerifySessionLingersAndServesNothing(t *testing.T) {
	base, _, wtTransport := wtTestServer(t)
	sess := dialWT(t, wtTransport, base+"/wt/download?bytes=0")

	ctx, cancel := context.WithTimeout(context.Background(), time.Second)
	defer cancel()
	if str, err := sess.AcceptUniStream(ctx); err == nil {
		t.Fatalf("verify session opened a stream: %v", str)
	}
	// A second's worth of accepting has passed, so a session that was going to
	// be torn down has been. Whatever ended the accept, it was not that.
	select {
	case <-sess.Context().Done():
		t.Fatal("verify session closed instead of lingering: its answer is the handshake, and the client is what closes it")
	default:
	}
}

// A stream download's liveness is the peer draining its lanes, and that is the
// only thing keeping the session open: the watchdog reaps one that carries
// nothing. With the bound cut far below the stage, a session being read has to
// outlive it several times over, or a lane writer that stopped counting its
// bytes would reap sessions at full throughput.
func TestDrainedStreamDownloadOutlivesTheIdleBound(t *testing.T) {
	const bound = 300 * time.Millisecond
	base, _, wtTransport := wtTestServerWithIdleBound(t, bound, nil)
	sess := dialWT(t, wtTransport, base+"/wt/download?bytes=262144&streams=1")

	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()
	start := time.Now()
	deadline := start.Add(6 * bound)
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

// An upload session carries only what the peer sends, and the one thing the
// server puts on it -- the progress feed -- heartbeats forever. So nothing
// about a silent upload session looks idle to the transport, and only the
// watchdog reclaims it. Without one it holds its slot for the session bound.
func TestIdleWebTransportUploadSessionFreesItsSlot(t *testing.T) {
	base, httpBase, wtTransport := wtTestServerWithIdleBound(t, 300*time.Millisecond, nil)

	// Dial an upload session with a real id and then send nothing at all.
	dialWT(t, wtTransport, base+"/wt/upload?id="+mintUploadID(t, httpBase))
	waitForLoad(t, httpBase, 1)
	waitForLoad(t, httpBase, 0)
}

// A byte stream carries no channel to report a refusal on, so the refusal is the
// reset. Whatever ended an upload lane -- a refused id, the idle bound, or a
// clean end -- the stream is reset, or bytes the server will never read sit in
// the session's flow-control window with the client parked on a write that
// nothing will ever drain.
func TestRefusedWebTransportUploadLaneIsReset(t *testing.T) {
	base, _, wtTransport := wtTestServer(t)
	sess := dialWT(t, wtTransport, base+"/wt/upload?id=gmu_nosuchupload")

	openCtx, cancelOpen := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancelOpen()
	lane, err := sess.OpenUniStreamSync(openCtx)
	if err != nil {
		t.Fatalf("open lane: %v", err)
	}
	// The deadline is the harness's only way out: an unreset lane parks on flow
	// control once the window fills, and parking forever reports nothing.
	if err := lane.SetWriteDeadline(time.Now().Add(15 * time.Second)); err != nil {
		t.Fatalf("lane write deadline: %v", err)
	}
	block := make([]byte, 64<<10)
	for {
		if _, err := lane.Write(block); err != nil {
			if errors.Is(err, os.ErrDeadlineExceeded) {
				t.Fatal("a refused upload lane was left open: the client parked on flow control instead of seeing the reset that is the only refusal a byte stream can carry")
			}
			return
		}
	}
}

// probeLoad reads the occupancy /probe reports. /probe is not admission-wrapped,
// so what it returns is the measurement routes' own held slots.
func probeLoad(t *testing.T, httpBase string) int {
	t.Helper()
	res, err := http.Get(httpBase + "/probe")
	if err != nil {
		t.Fatalf("probe: %v", err)
	}
	defer res.Body.Close()
	var body struct {
		Load *struct{ Active int } `json:"load"`
	}
	if err := json.NewDecoder(res.Body).Decode(&body); err != nil {
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

// A peer that stops reading and never closes still has to give its slot back.
// The budget cannot depend on a browser running its teardown: a terminated
// worker, a reloaded page and a dropped network all look like this, and before
// the idle bound they held a slot until the session lifetime expired.
func TestAbandonedWebTransportSessionFreesItsSlot(t *testing.T) {
	base, httpBase, wtTransport := wtTestServerWithIdleBound(t, 300*time.Millisecond, nil)

	// Never accept the lane. The server fills the flow-control window and then
	// nothing moves, which is what an abandoned session looks like from here.
	dialWT(t, wtTransport, base+"/wt/download?bytes=1073741824&streams=1")
	waitForLoad(t, httpBase, 1)
	waitForLoad(t, httpBase, 0)
}

// A datagram download is served entirely by the server: the peer sends nothing
// on it, and datagrams have neither flow control nor an application-level
// acknowledgement, so a client that stops reading and never closes looks exactly
// like one consuming at line rate. Counting the server's own sends as activity
// made that session immortal -- it held a session slot for the whole session
// bound and burned uplink the entire time. Only what the peer sends may keep a
// session alive, so an abandoned flood is reclaimed on the idle bound.
func TestAbandonedDatagramDownloadFreesItsSlot(t *testing.T) {
	base, httpBase, wtTransport := wtTestServerWithIdleBound(t, 300*time.Millisecond, nil)

	// Dial the flood and never read a datagram. A terminated worker, a
	// backgrounded tab and a hostile client all look like this from the server.
	dialWT(t, wtTransport, base+"/wt/download?bytes=2000&datagrams=1")
	waitForLoad(t, httpBase, 1)
	waitForLoad(t, httpBase, 0)
}

// The datagram ping bus is not a session route, so it keeps the request bound
// and the request bucket -- but an idle one still held its slot for that whole
// bound. QUIC's MaxIdleTimeout does not fire under keepalives, so a bus carrying
// nothing looks alive to the transport and only the watchdog reclaims it.
func TestIdleWebTransportPingSessionFreesItsSlot(t *testing.T) {
	base, httpBase, wtTransport := wtTestServerWithIdleBound(t, 300*time.Millisecond, nil)

	// Dial and then send nothing. A terminated worker, a reloaded page and a
	// dropped network all look like this from the server.
	dialWT(t, wtTransport, base+routeWTPing)
	waitForLoad(t, httpBase, 1)
	waitForLoad(t, httpBase, 0)
}

// wtOriginCheck is the ONLY origin policy a WebTransport CONNECT passes
// through. The auth boundary deliberately skips the ambient-credential origin
// rules for an extended CONNECT -- the credential there is non-ambient,
// short-lived and consumed on arrival -- so serveWebTransportConnect never
// consults originHeaderAllowed. Nothing else stands between a foreign page and
// a session, and no test had ever presented the upgrade an Origin that was not
// the server's own.
func TestWebTransportConnectRefusesAForeignOrigin(t *testing.T) {
	s := newAuthenticatedStack(t)

	foreign := http.Header{"Origin": {"https://attacker.example"}}
	res, sess, err := dialWTUntilAnswered(t, s.wtTransport(t), s.h3URL+routeWTPing+"?token="+url.QueryEscape(s.mintWTToken(t)), foreign)
	if err == nil {
		_ = sess.CloseWithError(0, "")
		t.Fatal("a CONNECT carrying a foreign Origin opened a session")
	}
	if res == nil {
		t.Fatalf("foreign-Origin CONNECT failed without a response: %v", err)
	}
	if res.StatusCode == http.StatusOK {
		t.Fatalf("foreign-Origin CONNECT status=%d, want a refusal", res.StatusCode)
	}

	// The control: the same credential, the same wtTransport and the same header, and
	// only the origin canonical. Without it a refusal of everything -- a broken
	// token, a rejected header -- would read as the origin policy working.
	res, sess, err = dialWTUntilAnswered(t, s.wtTransport(t), s.h3URL+routeWTPing+"?token="+url.QueryEscape(s.mintWTToken(t)), http.Header{"Origin": {s.origin}})
	if err != nil {
		t.Fatalf("CONNECT from the canonical origin was refused with status=%v: %v", res, err)
	}
	_ = sess.CloseWithError(0, "")
}

// dialWTUntilAnswered dials until the listener answers, so a QUIC listener
// still coming up is not read as a refusal. A response means the server
// answered: that is the outcome, whatever it says.
func dialWTUntilAnswered(t *testing.T, d *webtransport.Transport, target string, hdr http.Header) (*http.Response, *webtransport.Session, error) {
	t.Helper()
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
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

// wtTestServerTuned must not arm a cleanup that closes a wtTransport its caller never
// dialed: webtransport.Transport.Close cancels a context only the first Dial
// installs, and a panic in t.Cleanup aborts the whole test binary rather than
// one test -- under -shuffle=on, a different surviving set per seed.
func TestWTTestServerLeavesAnUnusedTransportAlone(t *testing.T) {
	// The assertion is what happens after this test returns: its cleanups run
	// with the wtTransport still unused.
	if base, httpBase, wtTransport := wtTestServerTuned(t, nil); base == "" || httpBase == "" || wtTransport == nil {
		t.Fatalf("helper returned base=%q httpBase=%q wtTransport=%v, want a usable server", base, httpBase, wtTransport)
	}
}

// A lane that keeps dropping redials, and the budget it needs is one however
// many times it does. Abandoning a session per attempt is what a terminated
// worker looks like, and before the idle bound the second attempt was refused.
func TestFlappingWebTransportLaneCostsOneSlot(t *testing.T) {
	base, httpBase, wtTransport := wtTestServerWithIdleBound(t, 300*time.Millisecond, func(c *config.Config) {
		c.MaxSessionsPerClient = 1
	})

	for attempt := 1; attempt <= 5; attempt++ {
		dialWT(t, wtTransport, base+"/wt/download?bytes=1073741824&streams=1")
		waitForLoad(t, httpBase, 1)
		waitForLoad(t, httpBase, 0)
	}
}

// TestWebTransportUploadCountsLanesOnItsProgressStream runs a whole upload over
// one session: the id is minted over HTTP, lanes are unidirectional streams, and
// the server-authoritative counter rides the one stream the server opens on
// that same session.
func TestWebTransportUploadCountsLanesOnItsProgressStream(t *testing.T) {
	base, httpBase, wtTransport := wtTestServer(t)

	// The id is minted and finalized over HTTP; only the bytes ride the session.
	client := http.DefaultClient
	id := mintUploadID(t, httpBase)
	sess := dialWT(t, wtTransport, base+"/wt/upload?id="+id)
	acceptCtx, cancelAccept := context.WithTimeout(context.Background(), 10*time.Second)
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
	lane, err := sess.OpenUniStreamSync(context.Background())
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

// TestGoClientRunsOverWebTransport drives the shipped client end to end on the
// transport it selects automatically: datagram pings, stream lanes, and the
// upload counter on the upload session.
func TestGoClientRunsOverWebTransport(t *testing.T) {
	_, httpBase := wtTestOrigins(t, nil)

	clientCfg := goclient.DefaultConfig()
	clientCfg.BaseURL = httpBase
	clientCfg.ThroughputTransport = "webtransport"
	clientCfg.InsecureSkipTLSVerify = true
	clientCfg.Stages = goclient.StageSet{Latency: true, Download: true, Upload: true}
	clientCfg.Warmup = 100 * time.Millisecond
	clientCfg.LatencyDuration = 300 * time.Millisecond
	clientCfg.DownloadDuration = 500 * time.Millisecond
	clientCfg.UploadDuration = 500 * time.Millisecond

	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()
	prepared, err := goclient.Prepare(ctx, clientCfg)
	if err != nil {
		t.Fatalf("prepare: %v", err)
	}
	if got := prepared.ThroughputTarget.Transport; got != wire.TransportWebTransport {
		t.Fatalf("throughput transport = %q, want webtransport", got)
	}
	if got := prepared.LatencyTarget.Transport; got != wire.TransportWebTransport {
		t.Fatalf("latency transport = %q, want webtransport", got)
	}

	results := map[string]goclient.Result{}
	err = goclient.RunPrepared(ctx, clientCfg, prepared, func(e goclient.Event) {
		if e.Kind == goclient.EventResult && e.Result != nil {
			results[e.Stage] = *e.Result
		}
	})
	if err != nil {
		t.Fatalf("run: %v", err)
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

// runGoClientUnderLifetimeCaps runs the shipped client against a server whose
// request and session bounds are far shorter than the stages, so every channel
// is killed mid-stage several times. Completion proves the reconnect paths.
func runGoClientUnderLifetimeCaps(t *testing.T, throughputTransport, latencyTransport string) {
	t.Helper()
	_, httpBase := wtTestOrigins(t, func(c *config.Config) {
		c.MaxOperationDuration = 2 * time.Second
		c.MaxSessionDuration = 2 * time.Second
	})

	clientCfg := goclient.DefaultConfig()
	clientCfg.BaseURL = httpBase
	clientCfg.ThroughputTransport = throughputTransport
	clientCfg.LatencyTransport = latencyTransport
	clientCfg.InsecureSkipTLSVerify = true
	clientCfg.Stages = goclient.StageSet{Latency: true, Download: true, Upload: true}
	clientCfg.Warmup = 100 * time.Millisecond
	clientCfg.LatencyDuration = 5 * time.Second
	clientCfg.DownloadDuration = 5 * time.Second
	clientCfg.UploadDuration = 5 * time.Second

	ctx, cancel := context.WithTimeout(context.Background(), 60*time.Second)
	defer cancel()
	results := map[string]goclient.Result{}
	err := goclient.Run(ctx, clientCfg, func(e goclient.Event) {
		if e.Kind == goclient.EventResult && e.Result != nil {
			results[e.Stage] = *e.Result
		}
	})
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

// TestGoClientOutlivesSessionBoundOverWebTransport: 5 s stages under a 2 s
// session cap force at least two session kills per stage, on the transfer
// session and the datagram ping bus alike.
func TestGoClientOutlivesSessionBoundOverWebTransport(t *testing.T) {
	t.Parallel()
	runGoClientUnderLifetimeCaps(t, "webtransport", "webtransport")
}

// TestGoClientOutlivesOperationBoundOverFetch: the WebSocket ping bus and the
// upload progress GET both die at the 2 s request cap and reconnect. The bus
// is named rather than left automatic, which would select WebTransport here
// and leave the WebSocket reconnect untested.
func TestGoClientOutlivesOperationBoundOverFetch(t *testing.T) {
	t.Parallel()
	runGoClientUnderLifetimeCaps(t, "fetch-stream", "websocket")
}

// wtObservedSession is what one WebTransport session looked like from inside the
// endpoint it drove: how many lanes it ran in total, and the most it ran at once.
type wtObservedSession struct{ lanes, live, peak int }

// wtLaneCounter wraps a transfer endpoint and records what the server itself saw
// on the WebTransport side of it. A session's context is its identity here --
// endpoint/webtransport.go builds one per accepted session and hands it to every
// lane on that session -- so a client that replaced its session shows up as a
// second entry, which is the only way this side of the wire can tell a recycled
// session from a reopened lane.
//
// The HTTP routes share these endpoints, so anything that is not a WebTransport
// session passes straight through uncounted.
type wtLaneCounter struct {
	endpoint.Endpoint
	// cut, when set, is consulted once per WebTransport lane and returns how long
	// that lane may carry bytes before the endpoint gives up on it; zero serves it
	// normally. Ending a lane early is what makes endpoint/webtransport.go reset
	// the client's stream, which is the only refusal a byte stream can carry. It
	// is set before assemble and never written again.
	cut func(lane int) time.Duration

	mu       sync.Mutex
	byCtx    map[context.Context]*wtObservedSession
	sessions []*wtObservedSession
	lanes    int
}

func (c *wtLaneCounter) Handle(s transport.Session) error {
	if s.Proto() != transport.ProtoWebTransport {
		return c.Endpoint.Handle(s)
	}
	obs, lane := c.enterLane(s.Context())
	defer c.leaveLane(obs)
	if c.cut != nil {
		if window := c.cut(lane); window > 0 {
			return c.Endpoint.Handle(cutLaneSession{Session: s, until: time.Now().Add(window)})
		}
	}
	return c.Endpoint.Handle(s)
}

func (c *wtLaneCounter) enterLane(ctx context.Context) (*wtObservedSession, int) {
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.byCtx == nil {
		c.byCtx = map[context.Context]*wtObservedSession{}
	}
	obs, ok := c.byCtx[ctx]
	if !ok {
		obs = &wtObservedSession{}
		c.byCtx[ctx] = obs
		c.sessions = append(c.sessions, obs)
	}
	c.lanes++
	obs.lanes++
	if obs.live++; obs.live > obs.peak {
		obs.peak = obs.live
	}
	return obs, c.lanes
}

func (c *wtLaneCounter) leaveLane(obs *wtObservedSession) {
	c.mu.Lock()
	defer c.mu.Unlock()
	obs.live--
}

// observed returns one entry per WebTransport session, in the order they first
// carried a lane.
func (c *wtLaneCounter) observed() []wtObservedSession {
	c.mu.Lock()
	defer c.mu.Unlock()
	out := make([]wtObservedSession, 0, len(c.sessions))
	for _, s := range c.sessions {
		out = append(out, *s)
	}
	return out
}

// reset forgets every session, so one server can serve several runs and each
// still be read on its own.
func (c *wtLaneCounter) reset() {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.byCtx, c.sessions, c.lanes = nil, nil, 0
}

// cutLaneSession serves a lane's source only until its deadline. The source
// ending is a clean end as far as the endpoint is concerned, and the
// WebTransport upload handler resets every lane it finishes with, so what the
// client meets is a stream reset mid-transfer. The bound is time rather than a
// byte count: a count lands at whatever moment the link happens to reach it.
type cutLaneSession struct {
	transport.Session
	until time.Time
}

func (s cutLaneSession) OpenUploadSource() (io.Reader, error) {
	src, err := s.Session.OpenUploadSource()
	if err != nil {
		return nil, err
	}
	return &deadlineSource{src: src, until: s.until}, nil
}

// ClientOwner is not part of transport.Session, so embedding one does not carry
// it. The upload endpoint reads the owner off the session by assertion, and a
// lost owner would refuse the lane as another client's rather than serve it.
func (s cutLaneSession) ClientOwner() string {
	if owner, ok := s.Session.(interface{ ClientOwner() string }); ok {
		return owner.ClientOwner()
	}
	return ""
}

type deadlineSource struct {
	src   io.Reader
	until time.Time
}

func (r *deadlineSource) Read(p []byte) (int, error) {
	if !time.Now().Before(r.until) {
		return 0, io.EOF
	}
	return r.src.Read(p)
}

// closeSessionBudget refuses every later WebTransport session with the 503 a
// saturated session budget answers.
//
// Parked sessions cannot hold the budget shut here, and the arithmetic is why:
// the client's own session, dying on the session bound, hands back the very slot
// its redial needs, so parked sessions can only ever occupy sessionMax-1 of the
// budget at that instant and whichever of the two dials the freed slot first is a
// race. Moving the ceiling instead reaches the same acquire() refusal, with the
// same status and the same Retry-After, at a moment the test chooses.
func closeSessionBudget(a *requestAdmission) {
	a.mu.Lock()
	defer a.mu.Unlock()
	a.sessionMax = 0
}

// wtClientConfig is the shipped client pointed at a test server over
// WebTransport, with the stage durations left to the caller. Loaded latency is
// off: these tests are about the transfer session, and a probe riding alongside
// it puts a second transport and a second per-stage result into the picture.
func wtClientConfig(httpBase string) goclient.Config {
	cfg := goclient.DefaultConfig()
	cfg.BaseURL = httpBase
	cfg.ThroughputTransport = wire.TransportWebTransport
	cfg.InsecureSkipTLSVerify = true
	cfg.LoadedLatency = false
	cfg.Warmup = 100 * time.Millisecond
	return cfg
}

// A session that is refused for the rest of a measured window has to fail the
// stage. The window's clock runs whether or not a session is up, so the bytes
// that did move divided by the window they did not move over is not a rate: this
// stage reported roughly a fifth of the truth as a completed result, MeanBps and
// all, and returned no error to say so. A wrong number published as a
// measurement is worse than no measurement.
func TestWebTransportStageFailsWhenTheSessionIsRefusedMidWindow(t *testing.T) {
	t.Parallel()
	_, httpBase, e := wtShapedServer(t, func(c *config.Config) {
		// The session bound kills the stage's session a fifth of the way into the
		// window; the closed budget is what stops it coming back.
		c.MaxOperationDuration = 2 * time.Second
		c.MaxSessionDuration = 2 * time.Second
	}, nil)

	clientCfg := wtClientConfig(httpBase)
	clientCfg.Stages = goclient.StageSet{Download: true}
	clientCfg.DownloadDuration = 10 * time.Second

	ctx, cancel := context.WithTimeout(context.Background(), 60*time.Second)
	defer cancel()
	var mu sync.Mutex
	closed, downloadResults := false, 0
	err := goclient.Run(ctx, clientCfg, func(ev goclient.Event) {
		mu.Lock()
		defer mu.Unlock()
		switch ev.Kind {
		case goclient.EventThroughput:
			// Bytes are moving inside the measured window, so the stage's own
			// session is established: shutting the budget now refuses the
			// replacement rather than the original.
			if !closed {
				closed = true
				closeSessionBudget(e.admission)
			}
		case goclient.EventResult:
			if ev.Stage == "download" {
				downloadResults++
			}
		}
	})

	mu.Lock()
	defer mu.Unlock()
	if !closed {
		t.Fatal("the stage never reported a throughput sample, so the session budget was never shut and nothing was tested")
	}
	if err == nil {
		t.Fatal("a download whose session was refused for the rest of its window returned no error: the shortfall is being published as a rate")
	}
	const want = "webtransport session lost and not replaced within 2s"
	if !strings.Contains(err.Error(), want) {
		t.Fatalf("stage err = %q, want it to name the unreplaced session (%q)", err, want)
	}
	if downloadResults != 0 {
		t.Fatalf("the failed download emitted %d result events; a stage that could not measure must publish no number at all", downloadResults)
	}
}

// The published lane range is 1..16 over WebTransport, and nothing ran the top of
// it end to end. These are whole client runs at 4 and 16 lanes per direction,
// against the single-lane run as the control.
func TestGoClientRunsMultipleLanesOverWebTransport(t *testing.T) {
	t.Parallel()
	down, up := &wtLaneCounter{}, &wtLaneCounter{}
	_, httpBase, _ := wtShapedServer(t, nil, func(e *endpoints) {
		down.Endpoint, up.Endpoint = e.download, e.upload
		e.download, e.upload = down, up
	})

	run := func(t *testing.T, streams int) map[string]goclient.Result {
		t.Helper()
		down.reset()
		up.reset()
		clientCfg := wtClientConfig(httpBase)
		clientCfg.Stages = goclient.StageSet{Download: true, Upload: true}
		clientCfg.DownloadDuration = 700 * time.Millisecond
		clientCfg.UploadDuration = 700 * time.Millisecond
		clientCfg.TransferStreams = goclient.TransferStreamPolicy{Forced: streams}

		ctx, cancel := context.WithTimeout(context.Background(), 60*time.Second)
		defer cancel()
		results := map[string]goclient.Result{}
		err := goclient.Run(ctx, clientCfg, func(ev goclient.Event) {
			if ev.Kind == goclient.EventResult && ev.Result != nil {
				results[ev.Stage] = *ev.Result
			}
		})
		if err != nil {
			t.Fatalf("run at %d lanes: %v", streams, err)
		}
		// The server opens the download lanes and the client opens the upload
		// ones, so both directions are checked against what the server saw.
		for name, counter := range map[string]*wtLaneCounter{"download": down, "upload": up} {
			observed := counter.observed()
			if len(observed) != 1 {
				t.Fatalf("%s at %d lanes ran on %d WebTransport sessions, want exactly 1", name, streams, len(observed))
			}
			if observed[0].peak != streams {
				t.Errorf("the server saw %d concurrent %s lanes at --streams %d, want %d", observed[0].peak, name, streams, streams)
			}
			if got := results[name].TotalBytes; got == 0 {
				t.Errorf("%s at %d lanes moved no bytes", name, streams)
			}
		}
		return results
	}

	var single map[string]goclient.Result
	t.Run("1", func(t *testing.T) { single = run(t, 1) })
	if single == nil {
		t.Fatal("the single-lane control did not run, so there is nothing to compare against")
	}
	for _, streams := range []int{4, 16} {
		t.Run(strconv.Itoa(streams), func(t *testing.T) {
			results := run(t, streams)
			// A loose band on purpose: loopback rates swing run to run, so what is
			// pinned is that splitting the transfer across lanes does not collapse
			// or run away with it, not any particular speedup.
			const factor = 10
			for _, stage := range []string{"download", "upload"} {
				got, base := results[stage].MeanBps, single[stage].MeanBps
				if base <= 0 {
					t.Fatalf("the single-lane %s control reported %v B/s, so the comparison is meaningless", stage, base)
				}
				if got < base/factor || got > base*factor {
					t.Errorf("%s at %d lanes ran at %.0f B/s against %.0f B/s on one lane, outside a factor of %d",
						stage, streams, got, base, factor)
				}
			}
		})
	}
}

// One lane's stream failing is not a lost session. Every lane shares the one
// session, so replacing it to serve a single lane's retry stops every sibling
// mid-transfer -- and the lane that faulted is the one thing that did not need
// it. Here the server resets exactly one lane mid-transfer and the session must
// carry on: the reset lane reopens on the same session, and the server sees no
// second one.
func TestWebTransportLaneResetLeavesTheSessionIntact(t *testing.T) {
	const lanes = 4
	up := &wtLaneCounter{}
	var cuts atomic.Int64
	// The second lane the session runs, cut once it has carried bytes for long
	// enough that the client reads the fault as a mid-transfer stream error
	// rather than a lane that never started.
	up.cut = func(lane int) time.Duration {
		if lane != 2 {
			return 0
		}
		cuts.Add(1)
		return 700 * time.Millisecond
	}
	_, httpBase, _ := wtShapedServer(t, nil, func(e *endpoints) {
		up.Endpoint = e.upload
		e.upload = up
	})

	clientCfg := wtClientConfig(httpBase)
	clientCfg.Stages = goclient.StageSet{Upload: true}
	clientCfg.UploadDuration = 3 * time.Second
	clientCfg.TransferStreams = goclient.TransferStreamPolicy{Forced: lanes}

	ctx, cancel := context.WithTimeout(context.Background(), 60*time.Second)
	defer cancel()
	results := map[string]goclient.Result{}
	err := goclient.Run(ctx, clientCfg, func(ev goclient.Event) {
		if ev.Kind == goclient.EventResult && ev.Result != nil {
			results[ev.Stage] = *ev.Result
		}
	})
	if err != nil {
		t.Fatalf("a single reset lane failed the whole upload stage: %v", err)
	}
	if cuts.Load() != 1 {
		t.Fatalf("the server reset %d lanes, want exactly 1: nothing about a single lane's fault was tested", cuts.Load())
	}

	observed := up.observed()
	if len(observed) != 1 {
		t.Fatalf("a reset lane cost the stage %d WebTransport sessions, want 1: replacing the session stops every sibling lane mid-transfer to serve the retry of the one that faulted", len(observed))
	}
	// The reset lane reopened on that same session: the original lanes plus at
	// least one replacement.
	if observed[0].lanes < lanes+1 {
		t.Errorf("the session ran %d lanes in total, want at least %d: the reset lane never came back", observed[0].lanes, lanes+1)
	}
	if observed[0].peak != lanes {
		t.Errorf("the server saw %d concurrent lanes, want %d", observed[0].peak, lanes)
	}
	if got := results["upload"].TotalBytes; got == 0 {
		t.Error("the sibling lanes moved no bytes across the reset")
	}
}

// firstProgressTypeIs reports whether the FIRST non-blank record has this type.
// The feed's opening record is part of the contract, so this asserts on it
// rather than scanning ahead for a match.
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
