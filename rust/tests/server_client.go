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
	"strconv"
	"strings"
	"time"
)

func dialPeer(ctx context.Context, host string, config *tls.Config, transport *webtransport.Transport) (*quic.Conn, *webtransport.ClientConn, error) {
	connection, err := quic.DialAddr(ctx, host, config, &quic.Config{EnableDatagrams: true, EnableStreamResetPartialDelivery: true})
	if err != nil {
		return nil, nil, err
	}
	peer, err := transport.NewClientConn(connection)
	if err != nil {
		connection.CloseWithError(0, "initialization failed")
		return nil, nil, err
	}
	return connection, peer, nil
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

// Prerequisite failures stop the probe while its deferred connection cleanup still runs.
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
func loopback(port string) (string, error) {
	number, err := strconv.ParseUint(port, 10, 16)
	if err != nil {
		return "", fmt.Errorf("invalid loopback port %q", port)
	}
	return fmt.Sprintf("https://127.0.0.1:%d", number), nil
}

func run() error {
	if len(os.Args) != 2 && len(os.Args) != 3 {
		return fmt.Errorf("usage: server_client H3_PORT [AUTH_TLS_PORT] (reads ca.pem from the working directory)")
	}
	cert := must(os.ReadFile("ca.pem"))
	roots := x509.NewCertPool()
	if !roots.AppendCertsFromPEM(cert) {
		return fmt.Errorf("invalid certificate")
	}
	config := &tls.Config{RootCAs: roots, MinVersion: tls.VersionTLS13}
	ctx, cancel := context.WithTimeout(context.Background(), 25*time.Second)
	defer cancel()
	base := must(loopback(os.Args[1]))
	if len(os.Args) == 3 {
		public := must(loopback(os.Args[2]))
		return runAuthenticated(ctx, base, public, config)
	}
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
	conn, client, err := dialPeer(ctx, target.Host, config, transport)
	check(err)
	defer conn.CloseWithError(0, "probe finished")
	defer transport.Close()
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
	mint := func() (string, error) {
		data := must(request("POST", "/upload/session", nil))
		var value struct {
			ID string `json:"uploadId"`
		}
		check(json.Unmarshal(data, &value))
		if value.ID == "" {
			return "", fmt.Errorf("empty upload ID")
		}
		return value.ID, nil
	}
	data := must(request("GET", "/download?bytes=65537", nil))
	if len(data) != 65537 {
		return fmt.Errorf("H3 download length %d", len(data))
	}
	id := must(mint())
	data = must(request("POST", "/upload?id="+url.QueryEscape(id), bytes.NewReader(bytes.Repeat([]byte("u"), 65537))))
	var uploaded struct {
		Bytes uint64 `json:"bytes"`
	}
	check(json.Unmarshal(data, &uploaded))
	if uploaded.Bytes != 65537 {
		return fmt.Errorf("H3 upload bytes %d", uploaded.Bytes)
	}
	fmt.Println("H3 application download/upload: 65537 bytes each")
	dial := func(path string) (*webtransport.Session, error) {
		// Per-session flow control is not negotiated: each concurrent session
		// needs its own QUIC connection. Ordinary H3 requests may still share it.
		connection, peer, err := dialPeer(ctx, target.Host, config, transport)
		if err != nil {
			return nil, err
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
	pong := func(session *webtransport.Session, id string) error {
		check(session.SendDatagram([]byte("PING," + id)))
		data := must(session.ReceiveDatagram(ctx))
		if !strings.HasPrefix(string(data), "PONG,"+id+",") {
			return fmt.Errorf("unexpected pong %q", data)
		}
		return nil
	}
	check(pong(ping, "41"))
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
	for range 2 {
		stream := must(download.AcceptUniStream(ctx))
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
	check(download.CloseWithError(7, "download finished"))
	if err = pong(ping, "42"); err != nil {
		return fmt.Errorf("close isolation: %w", err)
	}
	if _, err = request("GET", "/download?bytes=1", nil); err != nil {
		return fmt.Errorf("H3 request sharing ping connection: %w", err)
	}
	fmt.Println("WT ping and two download streams: exact lengths; independent ping connection survives close; H3 shares ping connection")
	id = must(mint())
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
	check(pong(ping, "43"))
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
	check(concurrentPing.SendDatagram([]byte("PING,44")))
	pingCtx, stopPing := context.WithTimeout(ctx, 2*time.Second)
	answer, err := concurrentPing.ReceiveDatagram(pingCtx)
	stopPing()
	if err != nil {
		return fmt.Errorf("WT ping during datagram download: %w", err)
	}
	if !strings.HasPrefix(string(answer), "PONG,44,") {
		return fmt.Errorf("WT ping during datagram download: unexpected reply %q", answer)
	}
	check(datagramDownload.CloseWithError(0, "download finished"))
	fmt.Println("WT datagram download: received at least 65537 payload bytes; concurrent ping replied")

	id = must(mint())
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

func runAuthenticated(ctx context.Context, base, public string, tlsConfig *tls.Config) error {
	tcp := &http.Transport{TLSClientConfig: tlsConfig}
	defer tcp.CloseIdleConnections()
	https := &http.Client{
		Transport:     tcp,
		CheckRedirect: func(*http.Request, []*http.Request) error { return http.ErrUseLastResponse },
	}
	request := func(method, target string, body io.Reader) (*http.Request, error) {
		return http.NewRequestWithContext(ctx, method, target, body)
	}
	login := must(request("GET", public+"/login", nil))
	loginResponse := must(https.Do(login))
	loginResponse.Body.Close()
	if loginResponse.StatusCode != http.StatusOK {
		return fmt.Errorf("login page status=%d", loginResponse.StatusCode)
	}
	loginNonce := responseCookie(loginResponse, "__Host-gm_login")
	if loginNonce == nil {
		return fmt.Errorf("login page omitted nonce cookie")
	}
	form := url.Values{"csrf": {loginNonce.Value}, "password": {"correct horse battery staple"}}
	password := must(request("POST", public+"/auth/password", strings.NewReader(form.Encode())))
	password.Header.Set("Origin", public)
	password.Header.Set("Content-Type", "application/x-www-form-urlencoded")
	password.AddCookie(loginNonce)
	signedIn := must(https.Do(password))
	signedIn.Body.Close()
	if signedIn.StatusCode != http.StatusSeeOther {
		return fmt.Errorf("password sign-in status=%d", signedIn.StatusCode)
	}
	session := responseCookie(signedIn, "__Host-gm_session")
	csrf := responseCookie(signedIn, "__Host-gm_csrf")
	if session == nil || csrf == nil {
		return fmt.Errorf("password sign-in omitted session or CSRF cookie")
	}
	fmt.Println("Password login: canonical HTTPS origin and session cookies")

	protected := func(method, target string, body io.Reader) (*http.Request, error) {
		req := must(request(method, target, body))
		req.Header.Set("Origin", public)
		req.Header.Set("Sec-Fetch-Site", "same-origin")
		req.Header.Set("X-CSRF-Token", csrf.Value)
		req.AddCookie(session)
		return req, nil
	}
	target := must(url.Parse(base))
	quicTLS := tlsConfig.Clone()
	quicTLS.NextProtos = []string{http3.NextProtoH3}
	transport := &webtransport.Transport{TLSClientConfig: quicTLS}
	defer transport.Close()
	connection, client, err := dialPeer(ctx, target.Host, quicTLS, transport)
	check(err)
	defer connection.CloseWithError(0, "probe finished")
	unauthorized := must(request("GET", base+"/download?bytes=1", nil))
	denied := must(client.RoundTrip(unauthorized))
	denied.Body.Close()
	if denied.StatusCode != http.StatusForbidden || denied.Header.Get("Graphite-Meter-Auth") != "required" {
		return fmt.Errorf("unauthenticated H3 download status=%d auth=%q", denied.StatusCode, denied.Header.Get("Graphite-Meter-Auth"))
	}
	allowed := must(protected("GET", base+"/download?bytes=65537", nil))
	download := must(client.RoundTrip(allowed))
	data, readErr := io.ReadAll(io.LimitReader(download.Body, 65538))
	download.Body.Close()
	if readErr != nil || download.StatusCode != http.StatusOK || len(data) != 65537 {
		return fmt.Errorf("authenticated H3 download status=%d bytes=%d: %v", download.StatusCode, len(data), readErr)
	}
	fmt.Println("Authenticated H3: missing cookie denied; same-origin cookie download 65537 bytes")

	mint := func() (string, error) {
		query := url.Values{"target": {base + "/wt/ping"}}
		req := must(protected("POST", public+"/wt/session?"+query.Encode(), nil))
		response := must(https.Do(req))
		defer response.Body.Close()
		if response.StatusCode != http.StatusOK {
			return "", fmt.Errorf("WT ticket status=%d", response.StatusCode)
		}
		var ticket struct {
			Token string `json:"token"`
		}
		check(json.UnmarshalRead(response.Body, &ticket))
		if ticket.Token == "" {
			return "", fmt.Errorf("empty WT ticket")
		}
		return ticket.Token, nil
	}
	ticket := must(mint())
	connect := func(peer *webtransport.ClientConn, token string) (*http.Response, *webtransport.Session, error) {
		return peer.Dial(ctx, base+"/wt/ping?token="+url.QueryEscape(token), http.Header{"Origin": {public}})
	}
	deniedConnect, rejected, err := connect(client, "invalid-ticket")
	if err == nil {
		rejected.CloseWithError(0, "unexpected admission")
		return fmt.Errorf("invalid WT ticket unexpectedly admitted a session")
	}
	if deniedConnect == nil || deniedConnect.StatusCode != http.StatusForbidden {
		return fmt.Errorf("invalid WT ticket response=%v: %w", deniedConnect, err)
	}
	_, ping, err := connect(client, ticket)
	if err != nil {
		return fmt.Errorf("valid WT ticket after denied CONNECT: %w", err)
	}
	check(ping.SendDatagram([]byte("PING,77")))
	pong, err := ping.ReceiveDatagram(ctx)
	if err != nil || !strings.HasPrefix(string(pong), "PONG,77,") {
		return fmt.Errorf("authenticated WT ping reply=%q: %v", pong, err)
	}
	check(ping.CloseWithError(0, "ping finished"))
	rejectTicket := func(token string) error {
		connection, peer, err := dialPeer(ctx, target.Host, quicTLS, transport)
		check(err)
		defer connection.CloseWithError(0, "probe finished")
		response, session, err := connect(peer, token)
		if err == nil {
			session.CloseWithError(0, "unexpected admission")
			return fmt.Errorf("WT ticket unexpectedly admitted a session")
		}
		if response == nil || response.StatusCode != http.StatusForbidden {
			return fmt.Errorf("rejected WT ticket response=%v: %w", response, err)
		}
		return nil
	}
	if err := rejectTicket(ticket); err != nil {
		return fmt.Errorf("consumed WT ticket: %w", err)
	}
	fmt.Println("Authenticated WT: denied CONNECT preserved same connection; one-use ticket admitted datagram ping, then replay was denied")

	unused := must(mint())
	logoutForm := url.Values{"csrf": {csrf.Value}}
	logout := must(protected("POST", public+"/auth/logout", strings.NewReader(logoutForm.Encode())))
	logout.Header.Set("Content-Type", "application/x-www-form-urlencoded")
	loggedOut := must(https.Do(logout))
	loggedOut.Body.Close()
	if loggedOut.StatusCode != http.StatusSeeOther {
		return fmt.Errorf("logout status=%d", loggedOut.StatusCode)
	}
	before := must(protected("GET", base+"/download?bytes=1", nil))
	denied = must(client.RoundTrip(before))
	denied.Body.Close()
	if denied.StatusCode != http.StatusForbidden {
		return fmt.Errorf("revoked H3 session status=%d", denied.StatusCode)
	}
	if err := rejectTicket(unused); err != nil {
		return fmt.Errorf("revoked WT ticket: %w", err)
	}
	fmt.Println("Logout: prior H3 cookie and unused WT ticket rejected")
	return nil
}

func responseCookie(response *http.Response, name string) *http.Cookie {
	for _, cookie := range response.Cookies() {
		if cookie.Name == name {
			return cookie
		}
	}
	return nil
}
