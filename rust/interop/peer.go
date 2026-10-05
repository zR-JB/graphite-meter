// Peer with the unchanged Go QUIC, HTTP/3 and WebTransport libraries for the assembled Rust server.
//
//	peer flow H3_PORT    bootstrap, transfers, sessions and per-source Retry
//	peer retry H3_PORT   Retry once a quarter of the connection capacity is used
//
// Both read ca.pem from the working directory.
package main

import (
	"bufio"
	"bytes"
	"context"
	"crypto/tls"
	"crypto/x509"
	"encoding/json/v2"
	"errors"
	"fmt"
	"io"
	"net"
	"net/http"
	"net/url"
	"os"
	"strconv"
	"strings"
	"sync"
	"time"

	"github.com/quic-go/quic-go"
	"github.com/quic-go/quic-go/http3"
	"github.com/quic-go/quic-go/qlog"
	"github.com/quic-go/quic-go/qlogwriter"
	"github.com/quic-go/webtransport-go"
)

// handshake records what a client connection's tracer saw before the server's first packet.
type handshake struct {
	mu       sync.Mutex
	initials int
	answered bool
	retried  bool
}

func (h *handshake) AddProducer() qlogwriter.Recorder   { return h }
func (h *handshake) SupportsSchemas(schema string) bool { return schema == qlog.EventSchema }
func (h *handshake) Close() error                       { return nil }
func (h *handshake) state() (initials int, retried bool) {
	h.mu.Lock()
	defer h.mu.Unlock()
	return h.initials, h.retried
}
func (h *handshake) tracer() func(context.Context, bool, quic.ConnectionID) qlogwriter.Trace {
	return func(context.Context, bool, quic.ConnectionID) qlogwriter.Trace { return h }
}

func (h *handshake) RecordEvent(event qlogwriter.Event) {
	h.mu.Lock()
	defer h.mu.Unlock()
	switch event := event.(type) {
	case qlog.PacketSent:
		if !h.answered && event.Header.PacketType == qlog.PacketTypeInitial {
			h.initials++
		}
	case qlog.PacketReceived:
		h.answered = true
		h.retried = h.retried || event.Header.PacketType == qlog.PacketTypeRetry
	}
}

func quicConfig(trace *handshake) *quic.Config {
	return &quic.Config{EnableDatagrams: true, EnableStreamResetPartialDelivery: true, Tracer: trace.tracer()}
}

func dialPeer(ctx context.Context, host string, config *tls.Config, transport *webtransport.Transport) (*quic.Conn, *webtransport.ClientConn, *handshake, error) {
	trace := &handshake{}
	connection, err := quic.DialAddr(ctx, host, config, quicConfig(trace))
	if err != nil {
		return nil, nil, nil, err
	}
	peer, err := transport.NewClientConn(connection)
	if err != nil {
		connection.CloseWithError(0, "initialization failed")
		return nil, nil, nil, err
	}
	return connection, peer, trace, nil
}

func uploadProgress(ctx context.Context, session *webtransport.Session) (*bufio.Scanner, error) {
	stream := must(session.AcceptUniStream(ctx))
	deadline, _ := ctx.Deadline()
	stream.SetReadDeadline(deadline)
	scanner := bufio.NewScanner(stream)
	kind, _, err := readProgress(scanner)
	if err != nil {
		return nil, err
	}
	if kind != "ready" {
		return nil, fmt.Errorf("expected upload ready, got %q", kind)
	}
	return scanner, nil
}

func readProgress(scanner *bufio.Scanner) (string, uint64, error) {
	for scanner.Scan() {
		if scanner.Text() == "" {
			continue
		}
		var event struct {
			Type    string `json:"type"`
			Bytes   uint64 `json:"bytes"`
			Message string `json:"message"`
		}
		if err := json.Unmarshal(scanner.Bytes(), &event); err != nil {
			return "", 0, err
		}
		if event.Type == "error" {
			return "", 0, fmt.Errorf("upload: %s", event.Message)
		}
		return event.Type, event.Bytes, nil
	}
	if err := scanner.Err(); err != nil {
		return "", 0, err
	}
	return "", 0, io.ErrUnexpectedEOF
}

func finishUpload(scanner *bufio.Scanner, request func(string, string, io.Reader) ([]byte, error), id string) (uint64, error) {
	if _, err := request("DELETE", "/upload/progress?id="+url.QueryEscape(id), nil); err != nil {
		return 0, err
	}
	for {
		kind, count, err := readProgress(scanner)
		if err != nil {
			return 0, err
		}
		if kind == "complete" {
			return count, nil
		}
	}
}

func pong(ctx context.Context, session *webtransport.Session, id string) error {
	if err := session.SendDatagram([]byte("PING," + id)); err != nil {
		return err
	}
	data, err := session.ReceiveDatagram(ctx)
	if err == nil && !strings.HasPrefix(string(data), "PONG,"+id+",") {
		err = fmt.Errorf("unexpected pong %q", data)
	}
	return err
}

// Prerequisite failures stop the peer while its deferred connection cleanup still runs.
func check(err error) {
	if err != nil {
		panic(err)
	}
}

func must[T any](value T, err error) T {
	check(err)
	return value
}

func main() {
	if err := run(); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
}

func run() error {
	if len(os.Args) != 3 || (os.Args[1] != "flow" && os.Args[1] != "retry") {
		return fmt.Errorf("usage: peer flow|retry H3_PORT (reads ca.pem from the working directory)")
	}
	port, err := strconv.ParseUint(os.Args[2], 10, 16)
	if err != nil {
		return fmt.Errorf("invalid loopback port %q", os.Args[2])
	}
	roots := x509.NewCertPool()
	if !roots.AppendCertsFromPEM(must(os.ReadFile("ca.pem"))) {
		return fmt.Errorf("invalid certificate")
	}
	// The post-quantum key share spreads the ClientHello over several Initial packets.
	config := &tls.Config{RootCAs: roots, MinVersion: tls.VersionTLS13,
		CurvePreferences: []tls.CurveID{tls.X25519MLKEM768, tls.X25519}}
	ctx, cancel := context.WithTimeout(context.Background(), 25*time.Second)
	defer cancel()
	base := fmt.Sprintf("https://127.0.0.1:%d", port)
	if os.Args[1] == "retry" {
		return runRetry(ctx, base, config)
	}
	return runFlow(ctx, base, config)
}

// runRetry dials from 127.0.0.2, which an idle server admits without Retry, then from 127.0.0.3, which needs
// Retry because the first connection holds a quarter of a four-connection capacity.
func runRetry(ctx context.Context, base string, config *tls.Config) error {
	config = config.Clone()
	config.NextProtos = []string{http3.NextProtoH3}
	target := must(net.ResolveUDPAddr("udp", must(url.Parse(base)).Host))
	dial := func(source string) (*quic.Conn, *handshake, error) {
		socket, err := net.ListenUDP("udp", &net.UDPAddr{IP: net.ParseIP(source)})
		if err != nil {
			return nil, nil, err
		}
		trace := &handshake{}
		connection, err := (&quic.Transport{Conn: socket}).Dial(ctx, target, config, quicConfig(trace))
		return connection, trace, err
	}
	first, trace, err := dial("127.0.0.2")
	check(err)
	defer first.CloseWithError(0, "peer finished")
	if _, retried := trace.state(); retried {
		return fmt.Errorf("an idle server answered the first handshake with Retry")
	}
	second, trace, err := dial("127.0.0.3")
	check(err)
	defer second.CloseWithError(0, "peer finished")
	if _, retried := trace.state(); !retried {
		return fmt.Errorf("a quarter of the connection capacity is used, but the handshake had no Retry")
	}
	request := must(http.NewRequestWithContext(ctx, "GET", base+"/download?bytes=1", nil))
	response := must((&http3.Transport{}).NewClientConn(second).RoundTrip(request))
	defer response.Body.Close()
	if data, err := io.ReadAll(response.Body); err != nil || response.StatusCode != 200 || len(data) != 1 {
		return fmt.Errorf("download after Retry status=%d bytes=%d: %v", response.StatusCode, len(data), err)
	}
	fmt.Println("Retry: none at idle from 127.0.0.2; required from 127.0.0.3 at a quarter of capacity; connection serves")
	return nil
}

func runFlow(ctx context.Context, base string, config *tls.Config) error {
	tcp := &http.Transport{TLSClientConfig: config}
	defer tcp.CloseIdleConnections()
	req, _ := http.NewRequestWithContext(ctx, "GET", base+"/probe", nil)
	response := must(tcp.RoundTrip(req))
	io.Copy(io.Discard, response.Body)
	response.Body.Close()
	if response.StatusCode != 200 || !strings.Contains(response.Header.Get("Alt-Svc"), "h3=") {
		return fmt.Errorf("bootstrap status=%d alt-svc=%q", response.StatusCode, response.Header.Get("Alt-Svc"))
	}
	fmt.Println("TCP HTTPS bootstrap: 200 and H3 Alt-Svc")
	target := must(url.Parse(base))
	config = config.Clone()
	config.NextProtos = []string{http3.NextProtoH3}
	transport := &webtransport.Transport{TLSClientConfig: config}
	conn, client, trace, err := dialPeer(ctx, target.Host, config, transport)
	check(err)
	defer conn.CloseWithError(0, "peer finished")
	defer transport.Close()
	if initials, _ := trace.state(); initials < 2 {
		return fmt.Errorf("the ClientHello took %d Initial packet; the check needs several", initials)
	}
	fmt.Println("Post-quantum ClientHello over several Initial packets: handshake complete")
	request := func(method, path string, body io.Reader) ([]byte, error) {
		req := must(http.NewRequestWithContext(ctx, method, base+path, body))
		resp := must(client.RoundTrip(req))
		defer resp.Body.Close()
		data := must(io.ReadAll(io.LimitReader(resp.Body, 2*1024*1024)))
		if resp.StatusCode < 200 || resp.StatusCode >= 300 {
			return nil, fmt.Errorf("%s %s: %d %q", method, path, resp.StatusCode, data)
		}
		return data, nil
	}
	mint := func() string {
		var value struct {
			ID string `json:"uploadId"`
		}
		check(json.Unmarshal(must(request("POST", "/upload/session", nil)), &value))
		if value.ID == "" {
			check(errors.New("empty upload ID"))
		}
		return value.ID
	}
	data := must(request("GET", "/download?bytes=65537", nil))
	if len(data) != 65537 {
		return fmt.Errorf("H3 download length %d", len(data))
	}
	id := mint()
	data = must(request("POST", "/upload?id="+url.QueryEscape(id), bytes.NewReader(bytes.Repeat([]byte("u"), 65537))))
	var uploaded struct {
		Bytes uint64 `json:"bytes"`
	}
	check(json.Unmarshal(data, &uploaded))
	if uploaded.Bytes != 65537 {
		return fmt.Errorf("H3 upload bytes %d", uploaded.Bytes)
	}
	fmt.Println("H3 application download/upload: 65537 bytes each")
	retries := 0
	dial := func(path string) (*webtransport.Session, error) {
		// Per-session flow control is not negotiated: each concurrent session
		// needs its own QUIC connection. Ordinary H3 requests may still share it.
		connection, peer, trace, err := dialPeer(ctx, target.Host, config, transport)
		if err != nil {
			return nil, err
		}
		if _, retried := trace.state(); retried {
			retries++
		}
		_, session, err := peer.Dial(ctx, base+path, nil)
		if err != nil {
			connection.CloseWithError(0, "session failed")
		}
		return session, err
	}
	_, ping, err := client.Dial(ctx, base+"/wt/ping", nil)
	check(err)
	defer ping.CloseWithError(0, "")
	check(pong(ctx, ping, "41"))
	_, extra, err := client.Dial(ctx, base+"/wt/ping", nil)
	if err == nil {
		extra.CloseWithError(0, "unexpected second session")
		return fmt.Errorf("server accepted concurrent WT sessions without session flow control")
	}
	streamErr, ok := errors.AsType[*quic.StreamError](err)
	if !ok || !streamErr.Remote || streamErr.ErrorCode != quic.StreamErrorCode(http3.ErrCodeRequestRejected) {
		return fmt.Errorf("expected remote H3_REQUEST_REJECTED for second WT session: %w", err)
	}
	fmt.Println("Second WT session without session flow control: rejected; connection remains usable")
	download := must(dial("/wt/download?bytes=65537&streams=2"))
	if retries != 1 {
		return fmt.Errorf("a second connection from a source holding one had no Retry")
	}
	fmt.Println("Per-source Retry: a second connection from 127.0.0.1 completed its handshake through Retry")
	for range 2 {
		stream := must(download.AcceptUniStream(ctx))
		deadline, _ := ctx.Deadline()
		stream.SetReadDeadline(deadline)
		if data, err := io.ReadAll(io.LimitReader(stream, 65538)); err != nil || len(data) != 65537 {
			return fmt.Errorf("WT download read %d bytes: %v", len(data), err)
		}
	}
	check(download.CloseWithError(7, "download finished"))
	if err = pong(ctx, ping, "42"); err != nil {
		return fmt.Errorf("close isolation: %w", err)
	}
	if _, err = request("GET", "/download?bytes=1", nil); err != nil {
		return fmt.Errorf("H3 request sharing ping connection: %w", err)
	}
	fmt.Println("WT ping and two download streams: exact lengths; independent ping connection survives close; H3 shares ping connection")
	id = mint()
	session := must(dial("/wt/upload?id=" + url.QueryEscape(id)))
	defer session.CloseWithError(0, "")
	progress := must(uploadProgress(ctx, session))
	deadline, _ := ctx.Deadline()
	lane := must(session.OpenUniStreamSync(ctx))
	lane.SetWriteDeadline(deadline)
	if _, err = lane.Write(bytes.Repeat([]byte("w"), 131073)); err != nil {
		return err
	}
	check(lane.Close())
	for {
		kind, count, err := readProgress(progress)
		check(err)
		if kind == "progress" && count == 131073 {
			break
		}
	}
	count := must(finishUpload(progress, request, id))
	if count != 131073 {
		return fmt.Errorf("WT complete bytes=%d", count)
	}
	check(pong(ctx, ping, "43"))
	check(ping.CloseWithError(0, "ping finished"))
	if _, err = request("GET", "/download?bytes=1", nil); err != nil {
		return fmt.Errorf("H3 request after sibling WT close: %w", err)
	}
	fmt.Println("WT upload: ready, measured progress, HTTP finish, complete=131073; H3 survives sibling WT close")

	datagramDownload := must(dial("/wt/download?bytes=65537&datagrams=1"))
	var received uint64
	for received < 65537 {
		payload, err := datagramDownload.ReceiveDatagram(ctx)
		if err != nil {
			return fmt.Errorf("WT datagram download after %d bytes: %w", received, err)
		}
		if len(payload) == 0 || len(payload) > 1000 {
			return fmt.Errorf("WT datagram download payload size %d", len(payload))
		}
		received += uint64(len(payload))
	}
	concurrentPing, err := dial("/wt/ping")
	if err != nil {
		return fmt.Errorf("WT ping during datagram download: %w", err)
	}
	defer concurrentPing.CloseWithError(0, "")
	pingCtx, stopPing := context.WithTimeout(ctx, 2*time.Second)
	err = pong(pingCtx, concurrentPing, "44")
	stopPing()
	if err != nil {
		return fmt.Errorf("WT ping during datagram download: %w", err)
	}
	check(datagramDownload.CloseWithError(0, "download finished"))
	fmt.Println("WT datagram download: received at least 65537 payload bytes; concurrent ping replied")

	id = mint()
	datagramUpload := must(dial("/wt/upload?datagrams=1&id=" + url.QueryEscape(id)))
	defer datagramUpload.CloseWithError(0, "")
	datagramProgress := must(uploadProgress(ctx, datagramUpload))
	const offered = 16 * 1000
	payload := bytes.Repeat([]byte("d"), 1000)
	for range 16 {
		check(datagramUpload.SendDatagram(payload))
	}
	var observed uint64
	for observed == 0 {
		kind, count, err := readProgress(datagramProgress)
		observed = count
		check(err)
		if kind != "progress" || observed > offered {
			return fmt.Errorf("WT datagram progress kind=%q bytes=%d", kind, observed)
		}
	}
	count = must(finishUpload(datagramProgress, request, id))
	if count < observed || count > offered {
		return fmt.Errorf("WT datagram complete bytes=%d, observed=%d", count, observed)
	}
	fmt.Println("WT datagram upload: ready, receiver progress, HTTP finish, bounded completion")
	return nil
}
