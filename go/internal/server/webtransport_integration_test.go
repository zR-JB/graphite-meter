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

	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()

	if err := sess.SendDatagram([]byte(wire.Encode(wire.Frame{Op: wire.OpHI, Proto: "wt"}))); err != nil {
		t.Fatalf("send HI: %v", err)
	}
	reply, err := sess.ReceiveDatagram(ctx)
	if err != nil {
		t.Fatalf("receive READY: %v", err)
	}
	if string(reply) != wire.OpREADY {
		t.Fatalf("HI reply = %q, want READY", reply)
	}

	if err := sess.SendDatagram([]byte(wire.Encode(wire.Frame{Op: wire.OpPING, ID: 42}))); err != nil {
		t.Fatalf("send PING: %v", err)
	}
	reply, err = sess.ReceiveDatagram(ctx)
	if err != nil {
		t.Fatalf("receive PONG: %v", err)
	}
	f, err := wire.Decode(string(reply))
	if err != nil {
		t.Fatalf("decode %q: %v", reply, err)
	}
	if f.Op != wire.OpPONG || f.ID != 42 {
		t.Fatalf("PING reply = %+v, want PONG with id 42", f)
	}
}

// TestWebTransportDownloadServesTheRequestedSize proves the SIZE preamble sizes
// exactly that lane's stream, and that a bad preamble leaves the session usable.
func TestWebTransportDownloadServesTheRequestedSize(t *testing.T) {
	base, _, dialer := wtTestServer(t)
	sess := dialWT(t, dialer, base+"/wt/download")

	const want = 1 << 20
	str, err := sess.OpenStreamSync(context.Background())
	if err != nil {
		t.Fatalf("open stream: %v", err)
	}
	if _, err := io.WriteString(str, wire.EncodeStreamPreamble(wire.Frame{Op: wire.OpSIZE, Bytes: want})); err != nil {
		t.Fatalf("write preamble: %v", err)
	}
	n, err := io.Copy(io.Discard, str)
	if err != nil {
		t.Fatalf("read lane: %v", err)
	}
	if n != want {
		t.Fatalf("lane served %d bytes, want %d", n, want)
	}

	bad, err := sess.OpenStreamSync(context.Background())
	if err != nil {
		t.Fatalf("open second stream: %v", err)
	}
	if _, err := io.WriteString(bad, "NOPE\n"); err != nil {
		t.Fatalf("write bad preamble: %v", err)
	}
	reply, err := io.ReadAll(bad)
	if err != nil {
		t.Fatalf("read rejection: %v", err)
	}
	if !strings.HasPrefix(string(reply), wire.OpERR+","+wire.ErrBadOp) {
		t.Fatalf("bad preamble reply = %q, want an ERR frame", reply)
	}
}

// TestWebTransportUploadCountsLanesOnItsProgressStream runs a whole upload over
// one session: the id is minted over HTTP, lanes are unidirectional streams, and
// the server-authoritative counter rides a bidirectional stream of that same
// session.
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
	progress, err := sess.OpenStreamSync(context.Background())
	if err != nil {
		t.Fatalf("open progress stream: %v", err)
	}
	if _, err := io.WriteString(progress, wire.EncodeStreamPreamble(wire.Frame{Op: wire.OpHI, Proto: "wt"})); err != nil {
		t.Fatalf("write progress preamble: %v", err)
	}
	records := bufio.NewScanner(progress)
	if !readProgressType(t, records, "ready") {
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

func readProgressType(t *testing.T, records *bufio.Scanner, want string) bool {
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
