package server

import (
	"bufio"
	"context"
	"crypto/tls"
	"encoding/json"
	"io"
	"net/http"
	"strings"
	"testing"
	"time"

	"github.com/quic-go/webtransport-go"
	"github.com/zR-JB/graphite-meter/go/internal/config"
	"github.com/zR-JB/graphite-meter/go/internal/goclient"
	"github.com/zR-JB/graphite-meter/go/internal/transport"
	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

// wtTestServer boots a server with HTTP/3 and returns the WebTransport base URL,
// the clear-H1 base URL for the HTTP half of an upload, and a session dialer.
func wtTestServer(t *testing.T) (string, string, *webtransport.Dialer) {
	t.Helper()
	cert, key := writeCertificate(t, t.TempDir(), "srv", "127.0.0.1",
		time.Now().Add(-time.Hour), time.Now().Add(time.Hour))
	cfg := config.Default()
	cfg.Native.H1 = freeTCPAddr(t)
	cfg.Native.H3 = freeTCPAddr(t)
	cfg.TLSCert, cfg.TLSKey = cert, key

	t.Cleanup(runUntilCancel(t, &cfg))
	waitForOK(t, http.DefaultClient, "http://"+cfg.Native.H1+"/preflight")

	dialer := &webtransport.Dialer{
		TLSClientConfig: &tls.Config{InsecureSkipVerify: true}, //nolint:gosec // self-signed test certificate
		QUICConfig:      transport.NewQUICConfig(),
	}
	t.Cleanup(func() { _ = dialer.Close() })
	return "https://" + cfg.Native.H3, "http://" + cfg.Native.H1, dialer
}

// dialWT opens a session, retrying while the QUIC listener finishes coming up.
func dialWT(t *testing.T, dialer *webtransport.Dialer, url string) *webtransport.Session {
	t.Helper()
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	for {
		_, sess, err := dialer.Dial(ctx, url, nil)
		if err == nil {
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

// TestWebTransportUploadDrainsDatagrams covers the datagram upload mode end to
// end: the server counts what arrives and reports it on the progress feed the
// same session carries.
func TestWebTransportUploadDrainsDatagrams(t *testing.T) {
	base, httpBase, dialer := wtTestServer(t)
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

	sess := dialWT(t, dialer, base+"/wt/upload?datagrams=1&id="+minted.UploadID)
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

// TestWebTransportUploadCountsLanesOnItsProgressStream runs a whole upload over
// one session: the id is minted over HTTP, lanes are unidirectional streams, and
// the server-authoritative counter rides the one stream the server opens on
// that same session.
func TestWebTransportUploadCountsLanesOnItsProgressStream(t *testing.T) {
	base, httpBase, dialer := wtTestServer(t)

	// The id is minted and finalized over HTTP; only the bytes ride the session.
	client := http.DefaultClient
	res, err := client.Post(httpBase+"/upload/session", "", nil)
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

	sess := dialWT(t, dialer, base+"/wt/upload?id="+minted.UploadID)
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

	req, err := http.NewRequest(http.MethodDelete, httpBase+"/upload/progress?id="+minted.UploadID, nil)
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
	cert, key := writeCertificate(t, t.TempDir(), "srv", "127.0.0.1",
		time.Now().Add(-time.Hour), time.Now().Add(time.Hour))
	cfg := config.Default()
	cfg.Native.H1 = freeTCPAddr(t)
	cfg.Native.H3 = freeTCPAddr(t)
	cfg.TLSCert, cfg.TLSKey = cert, key
	defer runUntilCancel(t, &cfg)()
	waitForOK(t, http.DefaultClient, "http://"+cfg.Native.H1+"/preflight")

	clientCfg := goclient.DefaultConfig()
	clientCfg.BaseURL = "http://" + cfg.Native.H1
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
	cert, key := writeCertificate(t, t.TempDir(), "srv", "127.0.0.1",
		time.Now().Add(-time.Hour), time.Now().Add(time.Hour))
	cfg := config.Default()
	cfg.Native.H1 = freeTCPAddr(t)
	cfg.Native.H3 = freeTCPAddr(t)
	cfg.TLSCert, cfg.TLSKey = cert, key
	cfg.MaxOperationDuration = 2 * time.Second
	cfg.MaxSessionDuration = 2 * time.Second
	defer runUntilCancel(t, &cfg)()
	waitForOK(t, http.DefaultClient, "http://"+cfg.Native.H1+"/preflight")

	clientCfg := goclient.DefaultConfig()
	clientCfg.BaseURL = "http://" + cfg.Native.H1
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
