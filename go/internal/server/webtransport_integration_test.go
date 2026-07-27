package server

import (
	"bufio"
	"context"
	"crypto/tls"
	"encoding/json"
	"io"
	"net/http"
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
// the clear-H1 base URL for the HTTP half of an upload, and a session dialer.
func wtTestServer(t *testing.T) (string, string, *testWTDialer) {
	t.Helper()
	return wtTestServerTuned(t, nil)
}

// wtTestServerTuned is wtTestServer with the config open to the caller.
func wtTestServerTuned(t *testing.T, tune func(*config.Config)) (string, string, *testWTDialer) {
	t.Helper()
	h3Base, httpBase := wtTestOrigins(t, tune)
	return h3Base, httpBase, &testWTDialer{Dialer: insecureWTDialer(), owner: t}
}

// testWTDialer is a session dialer whose close is armed only once a dial has
// landed. webtransport.Dialer.Close cancels a context only the first Dial
// installs, so closing one that never dialed dereferences nil -- and a panic in
// t.Cleanup aborts the whole test binary rather than failing one test, which
// under -shuffle=on leaves a different surviving set per seed.
type testWTDialer struct {
	*webtransport.Dialer
	owner *testing.T
	arm   sync.Once
}

// armClose ties the dialer to the test that asked for it, not to whichever
// subtest happened to dial first: several subtests share one dialer.
func (d *testWTDialer) armClose() {
	d.arm.Do(func() { d.owner.Cleanup(func() { _ = d.Dialer.Close() }) })
}

// wtTestOrigins boots the same server for a test that drives it through the
// shipped client rather than a raw session: the HTTP/3 origin and the clear-H1
// one, no dialer.
func wtTestOrigins(t *testing.T, tune func(*config.Config)) (string, string) {
	t.Helper()
	cert, key := writeCertificate(t, t.TempDir(), "srv", "127.0.0.1",
		time.Now().Add(-time.Hour), time.Now().Add(time.Hour))
	cfg := config.Default()
	cfg.Native.H1 = freeTCPAddr(t)
	cfg.Native.H3 = freeTCPAddr(t)
	cfg.TLSCert, cfg.TLSKey = cert, key
	if tune != nil {
		tune(&cfg)
	}

	t.Cleanup(runUntilCancel(t, &cfg))
	waitForOK(t, http.DefaultClient, "http://"+cfg.Native.H1+"/preflight")
	return "https://" + cfg.Native.H3, "http://" + cfg.Native.H1
}

// insecureWTDialer dials a test listener's self-signed certificate. The caller
// owns the dialer: most tie it to t, the stress harness to a goroutine.
func insecureWTDialer() *webtransport.Dialer {
	return &webtransport.Dialer{
		TLSClientConfig: &tls.Config{InsecureSkipVerify: true}, //nolint:gosec // self-signed test certificate
		QUICConfig:      transport.NewQUICConfig(),
	}
}

// dialWT opens a session, retrying while the QUIC listener finishes coming up.
func dialWT(t *testing.T, dialer *testWTDialer, url string) *webtransport.Session {
	t.Helper()
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	for {
		_, sess, err := dialer.Dial(ctx, url, nil)
		if err == nil {
			dialer.armClose()
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
	base, _, dialer := wtTestServer(t)
	sess := dialWT(t, dialer, base+"/wt/ping")

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
	base, _, dialer := wtTestServer(t)
	sess := dialWT(t, dialer, base+"/wt/download?bytes=1048576&streams=2")

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
	base, _, dialer := wtTestServer(t)
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
			sess := dialWT(t, dialer, base+"/wt/download?"+tc.query)
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
	base, httpBase, dialer := wtTestServer(t)
	sess := dialWT(t, dialer, base+"/wt/upload?id="+mintUploadID(t, httpBase))

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
	base, httpBase, dialer := wtTestServer(t)
	sess := dialWT(t, dialer, base+"/wt/upload?datagrams=1&id="+mintUploadID(t, httpBase))
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

// TestWebTransportDatagramFloodRepeats proves the flood re-runs while the
// session lives: more than one `bytes=` total arrives.
func TestWebTransportDatagramFloodRepeats(t *testing.T) {
	base, _, dialer := wtTestServer(t)
	sess := dialWT(t, dialer, base+"/wt/download?bytes=2000&datagrams=1")

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

// TestWebTransportVerifySessionServesNothing pins the bytes=0 transport check:
// the session establishes and no stream arrives.
func TestWebTransportVerifySessionServesNothing(t *testing.T) {
	base, _, dialer := wtTestServer(t)
	sess := dialWT(t, dialer, base+"/wt/download?bytes=0")

	ctx, cancel := context.WithTimeout(context.Background(), 300*time.Millisecond)
	defer cancel()
	if str, err := sess.AcceptUniStream(ctx); err == nil {
		t.Fatalf("verify session opened a stream: %v", str)
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
	defer endpoint.SetWTIdleBoundForTest(300 * time.Millisecond)()
	base, httpBase, dialer := wtTestServer(t)

	// Never accept the lane. The server fills the flow-control window and then
	// nothing moves, which is what an abandoned session looks like from here.
	dialWT(t, dialer, base+"/wt/download?bytes=1073741824&streams=1")
	waitForLoad(t, httpBase, 1)
	waitForLoad(t, httpBase, 0)
}

// The datagram ping bus is not a session route, so it keeps the request bound
// and the request bucket -- but an idle one still held its slot for that whole
// bound. QUIC's MaxIdleTimeout does not fire under keepalives, so a bus carrying
// nothing looks alive to the transport and only the watchdog reclaims it.
func TestIdleWebTransportPingSessionFreesItsSlot(t *testing.T) {
	defer endpoint.SetWTIdleBoundForTest(300 * time.Millisecond)()
	base, httpBase, dialer := wtTestServer(t)

	// Dial and then send nothing. A terminated worker, a reloaded page and a
	// dropped network all look like this from the server.
	dialWT(t, dialer, base+routeWTPing)
	waitForLoad(t, httpBase, 1)
	waitForLoad(t, httpBase, 0)
}

// wtTestServerTuned must not arm a cleanup that closes a dialer its caller never
// dialed: webtransport.Dialer.Close cancels a context only the first Dial
// installs, and a panic in t.Cleanup aborts the whole test binary rather than
// one test -- under -shuffle=on, a different surviving set per seed.
func TestWTTestServerLeavesAnUndialedDialerAlone(t *testing.T) {
	// The assertion is what happens after this test returns: its cleanups run
	// with the dialer still unused.
	if base, httpBase, dialer := wtTestServerTuned(t, nil); base == "" || httpBase == "" || dialer == nil {
		t.Fatalf("helper returned base=%q httpBase=%q dialer=%v, want a usable server", base, httpBase, dialer)
	}
}

// A lane that keeps dropping redials, and the budget it needs is one however
// many times it does. Abandoning a session per attempt is what a terminated
// worker looks like, and before the idle bound the second attempt was refused.
func TestFlappingWebTransportLaneCostsOneSlot(t *testing.T) {
	defer endpoint.SetWTIdleBoundForTest(300 * time.Millisecond)()
	base, httpBase, dialer := wtTestServerTuned(t, func(c *config.Config) {
		c.MaxSessionsPerClient = 1
	})

	for attempt := 1; attempt <= 5; attempt++ {
		dialWT(t, dialer, base+"/wt/download?bytes=1073741824&streams=1")
		waitForLoad(t, httpBase, 1)
		waitForLoad(t, httpBase, 0)
	}
}

// TestWebTransportUploadCountsLanesOnItsProgressStream runs a whole upload over
// one session: the id is minted over HTTP, lanes are unidirectional streams, and
// the server-authoritative counter rides the one stream the server opens on
// that same session.
func TestWebTransportUploadCountsLanesOnItsProgressStream(t *testing.T) {
	base, httpBase, dialer := wtTestServer(t)

	// The id is minted and finalized over HTTP; only the bytes ride the session.
	client := http.DefaultClient
	id := mintUploadID(t, httpBase)
	sess := dialWT(t, dialer, base+"/wt/upload?id="+id)
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
	runGoClientUnderLifetimeCaps(t, "webtransport", "webtransport")
}

// TestGoClientOutlivesOperationBoundOverFetch: the WebSocket ping bus and the
// upload progress GET both die at the 2 s request cap and reconnect. The bus
// is named rather than left automatic, which would select WebTransport here
// and leave the WebSocket reconnect untested.
func TestGoClientOutlivesOperationBoundOverFetch(t *testing.T) {
	runGoClientUnderLifetimeCaps(t, "fetch-stream", "websocket")
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
