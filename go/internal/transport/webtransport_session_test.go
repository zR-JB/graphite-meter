package transport

import (
	"bytes"
	"context"
	"errors"
	"io"
	"net/url"
	"strings"
	"testing"
)

// fakeDatagramConn replays queued datagrams and records what was sent.
type fakeDatagramConn struct {
	incoming []string
	sent     []string
	sendErr  error
}

func (c *fakeDatagramConn) SendDatagram(b []byte) error {
	if c.sendErr != nil {
		return c.sendErr
	}
	c.sent = append(c.sent, string(b))
	return nil
}

func (c *fakeDatagramConn) ReceiveDatagram(context.Context) ([]byte, error) {
	if len(c.incoming) == 0 {
		return nil, io.EOF
	}
	next := c.incoming[0]
	c.incoming = c.incoming[1:]
	return []byte(next), nil
}

func TestWebTransportBusSession(t *testing.T) {
	conn := &fakeDatagramConn{incoming: []string{"PING,1"}}
	sess := NewWebTransportBusSession(context.Background(), conn, url.Values{"id": {"gmu_x"}})

	if sess.Proto() != ProtoWebTransport {
		t.Errorf("Proto() = %v, want ProtoWebTransport", sess.Proto())
	}
	if got := sess.Query().Get("id"); got != "gmu_x" {
		t.Errorf("Query id = %q, want gmu_x", got)
	}
	if _, _, ok := sess.HTTP(); ok {
		t.Error("HTTP() ok = true, want false")
	}
	if _, err := sess.OpenDownloadSink(); !errors.Is(err, ErrUnsupported) {
		t.Errorf("OpenDownloadSink() err = %v, want ErrUnsupported", err)
	}
	if _, err := sess.OpenUploadSource(); !errors.Is(err, ErrUnsupported) {
		t.Errorf("OpenUploadSource() err = %v, want ErrUnsupported", err)
	}

	bus, ok := sess.Bus()
	if !ok {
		t.Fatal("Bus() ok = false, want true")
	}
	if bus.Reliable() {
		t.Error("Reliable() = true, want false: datagrams expose loss")
	}

	msg, err := bus.Recv()
	if err != nil {
		t.Fatalf("Recv: %v", err)
	}
	if msg != "PING,1" {
		t.Errorf("Recv = %q, want PING,1", msg)
	}
	if err := bus.Send("PONG,1;TIME,7"); err != nil {
		t.Fatalf("Send: %v", err)
	}
	if len(conn.sent) != 1 || conn.sent[0] != "PONG,1;TIME,7" {
		t.Errorf("sent = %v, want [PONG,1;TIME,7]", conn.sent)
	}
	if _, err := bus.Recv(); !errors.Is(err, io.EOF) {
		t.Errorf("Recv after drain = %v, want EOF", err)
	}
}

func TestWebTransportStreamSession(t *testing.T) {
	var sink bytes.Buffer
	src := strings.NewReader("payload")
	sess := NewWebTransportStreamSession(context.Background(), url.Values{"bytes": {"64"}}, &sink, src, "client-key")

	if _, ok := sess.Bus(); ok {
		t.Error("Bus() ok = true, want false")
	}

	w, err := sess.OpenDownloadSink()
	if err != nil {
		t.Fatalf("OpenDownloadSink: %v", err)
	}
	if _, err := io.WriteString(w, "bytes"); err != nil {
		t.Fatalf("write sink: %v", err)
	}
	if sink.String() != "bytes" {
		t.Errorf("sink = %q, want bytes", sink.String())
	}

	r, err := sess.OpenUploadSource()
	if err != nil {
		t.Fatalf("OpenUploadSource: %v", err)
	}
	got, err := io.ReadAll(r)
	if err != nil {
		t.Fatalf("read source: %v", err)
	}
	if string(got) != "payload" {
		t.Errorf("source = %q, want payload", got)
	}

	owner, ok := sess.(interface{ ClientOwner() string })
	if !ok {
		t.Fatal("stream session does not expose ClientOwner")
	}
	if owner.ClientOwner() != "client-key" {
		t.Errorf("ClientOwner() = %q, want client-key", owner.ClientOwner())
	}
}

func TestWebTransportStreamSessionUnusedSeams(t *testing.T) {
	sess := NewWebTransportStreamSession(context.Background(), nil, nil, nil, "")

	if _, err := sess.OpenDownloadSink(); !errors.Is(err, ErrUnsupported) {
		t.Errorf("OpenDownloadSink() err = %v, want ErrUnsupported", err)
	}
	if _, err := sess.OpenUploadSource(); !errors.Is(err, ErrUnsupported) {
		t.Errorf("OpenUploadSource() err = %v, want ErrUnsupported", err)
	}
}
