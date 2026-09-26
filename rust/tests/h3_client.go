// This probe uses the repository's unchanged Go dependencies as an independent peer.
package main

import (
	"context"
	"crypto/tls"
	"crypto/x509"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"os"
	"strconv"
	"strings"
	"time"

	"github.com/quic-go/quic-go"
	"github.com/quic-go/quic-go/http3"
	"github.com/quic-go/webtransport-go"
)

var base string

func main() {
	if err := run(); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
}

func run() error {
	if len(os.Args) != 2 {
		return fmt.Errorf("usage: h3_client PORT (reads cert.pem from the working directory)")
	}
	port, err := strconv.ParseUint(os.Args[1], 10, 16)
	if err != nil {
		return fmt.Errorf("invalid loopback port %q", os.Args[1])
	}
	base = fmt.Sprintf("https://127.0.0.1:%d", port)
	cert, err := os.ReadFile("cert.pem")
	if err != nil {
		return err
	}
	roots := x509.NewCertPool()
	if !roots.AppendCertsFromPEM(cert) {
		return fmt.Errorf("invalid certificate")
	}
	tlsConfig := &tls.Config{RootCAs: roots, MinVersion: tls.VersionTLS13}
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	tr := &http3.Transport{TLSClientConfig: tlsConfig}
	defer tr.Close()
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, base+"/probe", nil)
	if err != nil {
		return err
	}
	resp, err := tr.RoundTrip(req)
	if err != nil {
		return err
	}
	body, err := io.ReadAll(io.LimitReader(resp.Body, 1024))
	resp.Body.Close()
	if err != nil {
		return err
	}
	if resp.StatusCode != http.StatusOK || string(body) != "transport probe\n" {
		return fmt.Errorf("unexpected HTTP3 response: %d %q", resp.StatusCode, body)
	}
	fmt.Println("HTTP3: 200 transport probe")
	wt := &webtransport.Transport{TLSClientConfig: tlsConfig}
	defer wt.Close()
	for _, path := range []string{"ping", "download", "upload"} {
		if err := probeSession(ctx, wt, path); err != nil {
			return fmt.Errorf("WebTransport %s: %w", path, err)
		}
		fmt.Printf("WebTransport %s: passed\n", path)
	}
	if err := probeSharedConnection(ctx, wt, tlsConfig); err != nil {
		return fmt.Errorf("WebTransport shared connection: %w", err)
	}
	fmt.Println("WebTransport shared connection: nonzero sessions, isolated close, remote close, stream reset passed")
	return nil
}

func probeSession(ctx context.Context, wt *webtransport.Transport, path string) error {
	_, session, err := wt.Dial(ctx, base+"/wt/"+path, nil)
	if err != nil {
		return err
	}
	defer session.CloseWithError(0, "probe complete")
	if path == "ping" {
		if err := session.SendDatagram([]byte("PING,42")); err != nil {
			return err
		}
		reply, err := session.ReceiveDatagram(ctx)
		if err != nil {
			return err
		}
		if !strings.HasPrefix(string(reply), "PONG,42,") {
			return fmt.Errorf("unexpected pong: %q", reply)
		}
		return nil
	}
	payload := "webtransport " + path + "\n"
	if path == "upload" {
		stream, err := session.OpenUniStreamSync(ctx)
		if err != nil {
			return err
		}
		deadline, _ := ctx.Deadline()
		stream.SetWriteDeadline(deadline)
		if _, err := io.WriteString(stream, payload); err != nil {
			return err
		}
		if err := stream.Close(); err != nil {
			return err
		}
	}
	stream, err := session.AcceptUniStream(ctx)
	if err != nil {
		return err
	}
	deadline, _ := ctx.Deadline()
	stream.SetReadDeadline(deadline)
	body, err := io.ReadAll(io.LimitReader(stream, 1024))
	if err != nil {
		return err
	}
	if string(body) != payload {
		return fmt.Errorf("unexpected stream payload: %q", body)
	}
	return nil
}

func probeSharedConnection(ctx context.Context, wt *webtransport.Transport, tlsConfig *tls.Config) error {
	target, err := url.Parse(base)
	if err != nil {
		return err
	}
	config := tlsConfig.Clone()
	config.NextProtos = []string{http3.NextProtoH3}
	conn, err := quic.DialAddr(ctx, target.Host, config, &quic.Config{
		EnableDatagrams:                  true,
		EnableStreamResetPartialDelivery: true,
	})
	if err != nil {
		return err
	}
	defer conn.CloseWithError(0, "shared probe complete")
	client, err := wt.NewClientConn(conn)
	if err != nil {
		return err
	}
	// Consume request stream zero, so every session below has a nonzero ID.
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, base+"/probe", nil)
	if err != nil {
		return err
	}
	response, err := client.RoundTrip(req)
	if err != nil {
		return err
	}
	body, err := io.ReadAll(io.LimitReader(response.Body, 1024))
	response.Body.Close()
	if err != nil {
		return err
	}
	if response.StatusCode != http.StatusOK || string(body) != "transport probe\n" {
		return fmt.Errorf("shared HTTP3 response: %d %q", response.StatusCode, body)
	}
	_, first, err := client.Dial(ctx, base+"/wt/ping", nil)
	if err != nil {
		return err
	}
	defer first.CloseWithError(0, "probe complete")
	_, second, err := client.Dial(ctx, base+"/wt/ping", nil)
	if err != nil {
		return err
	}
	defer second.CloseWithError(0, "probe complete")
	// A stalled upload must not block this session's datagram lane or other sessions.
	stalled, err := first.OpenUniStreamSync(ctx)
	if err != nil {
		return err
	}
	defer stalled.CancelWrite(0)
	deadline, _ := ctx.Deadline()
	if err := stalled.SetWriteDeadline(deadline); err != nil {
		return err
	}
	if _, err := io.WriteString(stalled, "unfinished upload"); err != nil {
		return err
	}
	// Queue distinct payloads before receiving, detecting crossed session queues.
	if err := first.SendDatagram([]byte("PING,101")); err != nil {
		return err
	}
	if err := second.SendDatagram([]byte("PING,202")); err != nil {
		return err
	}
	if err := expectPong(ctx, first, "101"); err != nil {
		return err
	}
	if err := expectPong(ctx, second, "202"); err != nil {
		return err
	}
	if err := first.CloseWithError(19, "first session done"); err != nil {
		return err
	}
	if err := second.SendDatagram([]byte("PING,203")); err != nil {
		return err
	}
	if err := expectPong(ctx, second, "203"); err != nil {
		return err
	}
	_, closing, err := client.Dial(ctx, base+"/wt/close", nil)
	if err != nil {
		return err
	}
	defer closing.CloseWithError(0, "probe complete")
	// Incoming stream queues preserve the session close error; datagrams expose the raw QUIC cancellation.
	_, err = closing.AcceptUniStream(ctx)
	closeErr, ok := errors.AsType[*webtransport.SessionError](err)
	if !ok || !closeErr.Remote || closeErr.ErrorCode != 17 || closeErr.Message != "probe closed" {
		return fmt.Errorf("remote close: expected remote code 17 message probe closed, got %v", err)
	}
	if err := second.SendDatagram([]byte("PING,204")); err != nil {
		return err
	}
	if err := expectPong(ctx, second, "204"); err != nil {
		return err
	}
	_, resetting, err := client.Dial(ctx, base+"/wt/reset", nil)
	if err != nil {
		return err
	}
	defer resetting.CloseWithError(0, "probe complete")
	resetStream, err := resetting.AcceptUniStream(ctx)
	if err != nil {
		return fmt.Errorf("accept reset stream: %w", err)
	}
	if err := resetStream.SetReadDeadline(deadline); err != nil {
		return err
	}
	_, err = io.ReadAll(io.LimitReader(resetStream, 1024))
	streamErr, ok := errors.AsType[*webtransport.StreamError](err)
	if !ok || !streamErr.Remote || streamErr.ErrorCode != 7 {
		return fmt.Errorf("remote stream reset: expected remote code 7, got %v", err)
	}
	if err := second.SendDatagram([]byte("PING,205")); err != nil {
		return err
	}
	return expectPong(ctx, second, "205")
}

func expectPong(ctx context.Context, session *webtransport.Session, id string) error {
	reply, err := session.ReceiveDatagram(ctx)
	if err != nil {
		return err
	}
	if !strings.HasPrefix(string(reply), "PONG,"+id+",") {
		return fmt.Errorf("session pong %s: got %q", id, reply)
	}
	return nil
}
