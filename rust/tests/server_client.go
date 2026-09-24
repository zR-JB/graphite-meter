// Independent unchanged-Go-dependency peer for the assembled Rust application.
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
	"github.com/quic-go/quic-go"
	"github.com/quic-go/quic-go/http3"
	"github.com/quic-go/webtransport-go"
	"io"
	"net/http"
	"net/url"
	"os"
	"strings"
	"time"
)

func main() {
	if err := run(); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
}
func run() error {
	if len(os.Args) != 3 {
		return fmt.Errorf("usage: server_client URL CERT")
	}
	cert, err := os.ReadFile(os.Args[2])
	if err != nil {
		return err
	}
	roots := x509.NewCertPool()
	if !roots.AppendCertsFromPEM(cert) {
		return fmt.Errorf("invalid certificate")
	}
	config := &tls.Config{RootCAs: roots, MinVersion: tls.VersionTLS13}
	ctx, cancel := context.WithTimeout(context.Background(), 25*time.Second)
	defer cancel()
	base := os.Args[1]
	tcp := &http.Transport{TLSClientConfig: config}
	defer tcp.CloseIdleConnections()
	req, _ := http.NewRequestWithContext(ctx, "GET", base+"/probe", nil)
	response, err := tcp.RoundTrip(req)
	if err != nil {
		return err
	}
	io.Copy(io.Discard, response.Body)
	response.Body.Close()
	if response.StatusCode != 200 || !strings.Contains(response.Header.Get("Alt-Svc"), "h3=") {
		return fmt.Errorf("bootstrap status=%d alt-svc=%q", response.StatusCode, response.Header.Get("Alt-Svc"))
	}
	fmt.Println("TCP HTTPS bootstrap: 200 and H3 Alt-Svc")
	target, err := url.Parse(base)
	if err != nil {
		return err
	}
	config = config.Clone()
	config.NextProtos = []string{http3.NextProtoH3}
	conn, err := quic.DialAddr(ctx, target.Host, config, &quic.Config{EnableDatagrams: true, EnableStreamResetPartialDelivery: true})
	if err != nil {
		return err
	}
	defer conn.CloseWithError(0, "probe finished")
	transport := &webtransport.Transport{TLSClientConfig: config}
	defer transport.Close()
	client, err := transport.NewClientConn(conn)
	if err != nil {
		return err
	}
	request := func(method, path string, body io.Reader) ([]byte, error) {
		req, err := http.NewRequestWithContext(ctx, method, base+path, body)
		if err != nil {
			return nil, err
		}
		resp, err := client.RoundTrip(req)
		if err != nil {
			return nil, err
		}
		defer resp.Body.Close()
		data, err := io.ReadAll(io.LimitReader(resp.Body, 2*1024*1024))
		if err != nil {
			return nil, err
		}
		if resp.StatusCode < 200 || resp.StatusCode >= 300 {
			return nil, fmt.Errorf("%s %s: %d %q", method, path, resp.StatusCode, data)
		}
		return data, nil
	}
	mint := func() (string, error) {
		data, err := request("POST", "/upload/session", nil)
		if err != nil {
			return "", err
		}
		var value struct {
			ID string `json:"uploadId"`
		}
		err = json.Unmarshal(data, &value)
		if err == nil && value.ID == "" {
			err = fmt.Errorf("empty upload ID")
		}
		return value.ID, err
	}
	data, err := request("GET", "/download?bytes=65537", nil)
	if err != nil {
		return err
	}
	if len(data) != 65537 {
		return fmt.Errorf("H3 download length %d", len(data))
	}
	id, err := mint()
	if err != nil {
		return err
	}
	data, err = request("POST", "/upload?id="+url.QueryEscape(id), bytes.NewReader(bytes.Repeat([]byte("u"), 65537)))
	if err != nil {
		return err
	}
	var uploaded struct {
		Bytes uint64 `json:"bytes"`
	}
	if err = json.Unmarshal(data, &uploaded); err != nil {
		return err
	}
	if uploaded.Bytes != 65537 {
		return fmt.Errorf("H3 upload bytes %d", uploaded.Bytes)
	}
	fmt.Println("H3 application download/upload: 65537 bytes each")
	dial := func(path string) (*webtransport.Session, error) {
		// Per-session flow control is not negotiated: each concurrent session
		// needs its own QUIC connection. Ordinary H3 requests may still share it.
		connection, err := quic.DialAddr(ctx, target.Host, config, &quic.Config{EnableDatagrams: true, EnableStreamResetPartialDelivery: true})
		if err != nil {
			return nil, err
		}
		peer, err := transport.NewClientConn(connection)
		if err != nil {
			connection.CloseWithError(0, "initialization failed")
			return nil, err
		}
		_, session, err := peer.Dial(ctx, base+path, nil)
		if err != nil {
			connection.CloseWithError(0, "session failed")
		}
		return session, err
	}
	_, ping, err := client.Dial(ctx, base+"/wt/ping", nil)
	if err != nil {
		return err
	}
	defer ping.CloseWithError(0, "")
	pong := func(session *webtransport.Session, id string) error {
		if err := session.SendDatagram([]byte("PING," + id)); err != nil {
			return err
		}
		data, err := session.ReceiveDatagram(ctx)
		if err != nil {
			return err
		}
		if !strings.HasPrefix(string(data), "PONG,"+id+",") {
			return fmt.Errorf("unexpected pong %q", data)
		}
		return nil
	}
	if err = pong(ping, "41"); err != nil {
		return err
	}
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
	download, err := dial("/wt/download?bytes=65537&streams=2")
	if err != nil {
		return err
	}
	for range 2 {
		stream, err := download.AcceptUniStream(ctx)
		if err != nil {
			return err
		}
		deadline, _ := ctx.Deadline()
		stream.SetReadDeadline(deadline)
		data, err := io.ReadAll(io.LimitReader(stream, 65538))
		if err != nil {
			return fmt.Errorf("WT download read: %w", err)
		}
		if len(data) != 65537 {
			return fmt.Errorf("WT download length %d", len(data))
		}
	}
	if err = download.CloseWithError(7, "download finished"); err != nil {
		return err
	}
	if err = pong(ping, "42"); err != nil {
		return fmt.Errorf("close isolation: %w", err)
	}
	if _, err = request("GET", "/download?bytes=1", nil); err != nil {
		return fmt.Errorf("H3 request sharing ping connection: %w", err)
	}
	fmt.Println("WT ping and two download streams: exact lengths; independent ping connection survives close; H3 shares ping connection")
	id, err = mint()
	if err != nil {
		return err
	}
	session, err := dial("/wt/upload?id=" + url.QueryEscape(id))
	if err != nil {
		return err
	}
	defer session.CloseWithError(0, "")
	progress, err := session.AcceptUniStream(ctx)
	if err != nil {
		return err
	}
	deadline, _ := ctx.Deadline()
	progress.SetReadDeadline(deadline)
	scanner := bufio.NewScanner(progress)
	record := func(scanner *bufio.Scanner) (string, uint64, error) {
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
	kind, _, err := record(scanner)
	if err != nil {
		return err
	}
	if kind != "ready" {
		return fmt.Errorf("expected ready, got %q", kind)
	}
	lane, err := session.OpenUniStreamSync(ctx)
	if err != nil {
		return err
	}
	lane.SetWriteDeadline(deadline)
	if _, err = lane.Write(bytes.Repeat([]byte("w"), 131073)); err != nil {
		return err
	}
	if err = lane.Close(); err != nil {
		return err
	}
	for {
		kind, count, err := record(scanner)
		if err != nil {
			return err
		}
		if kind == "progress" && count == 131073 {
			break
		}
	}
	if _, err = request("DELETE", "/upload/progress?id="+url.QueryEscape(id), nil); err != nil {
		return err
	}
	for {
		kind, count, err := record(scanner)
		if err != nil {
			return err
		}
		if kind == "complete" {
			if count != 131073 {
				return fmt.Errorf("WT complete bytes=%d", count)
			}
			break
		}
	}
	if err = pong(ping, "43"); err != nil {
		return err
	}
	if err = ping.CloseWithError(0, "ping finished"); err != nil {
		return err
	}
	if _, err = request("GET", "/download?bytes=1", nil); err != nil {
		return fmt.Errorf("H3 request after sibling WT close: %w", err)
	}
	fmt.Println("WT upload: ready, measured progress, HTTP finish, complete=131073; H3 survives sibling WT close")

	datagramDownload, err := dial("/wt/download?bytes=65537&datagrams=1")
	if err != nil {
		return err
	}
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
	if err := datagramDownload.CloseWithError(0, "download finished"); err != nil {
		return err
	}
	fmt.Println("WT datagram download: received at least 65537 payload bytes")

	id, err = mint()
	if err != nil {
		return err
	}
	datagramUpload, err := dial("/wt/upload?datagrams=1&id=" + url.QueryEscape(id))
	if err != nil {
		return err
	}
	defer datagramUpload.CloseWithError(0, "")
	datagramProgress, err := datagramUpload.AcceptUniStream(ctx)
	if err != nil {
		return err
	}
	datagramProgress.SetReadDeadline(deadline)
	datagramScanner := bufio.NewScanner(datagramProgress)
	kind, _, err = record(datagramScanner)
	if err != nil {
		return err
	}
	if kind != "ready" {
		return fmt.Errorf("expected datagram upload ready, got %q", kind)
	}
	const offered = 16 * 1000
	payload := bytes.Repeat([]byte("d"), 1000)
	for range 16 {
		if err := datagramUpload.SendDatagram(payload); err != nil {
			return err
		}
	}
	var observed uint64
	for observed == 0 {
		kind, observed, err = record(datagramScanner)
		if err != nil {
			return err
		}
		if kind != "progress" || observed > offered {
			return fmt.Errorf("WT datagram progress kind=%q bytes=%d", kind, observed)
		}
	}
	if _, err = request("DELETE", "/upload/progress?id="+url.QueryEscape(id), nil); err != nil {
		return err
	}
	for {
		kind, count, err := record(datagramScanner)
		if err != nil {
			return err
		}
		if kind == "complete" {
			if count < observed || count > offered {
				return fmt.Errorf("WT datagram complete bytes=%d, observed=%d", count, observed)
			}
			break
		}
	}
	fmt.Println("WT datagram upload: ready, receiver progress, HTTP finish, bounded completion")
	return nil
}
