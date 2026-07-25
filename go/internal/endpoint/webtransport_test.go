package endpoint

import (
	"bufio"
	"context"
	"encoding/json"
	"io"
	"strings"
	"testing"
	"time"
)

// recordingConn is the datagram half of a session: it replays queued datagrams
// and records what was sent.
type recordingConn struct {
	incoming []string
	sent     [][]byte
}

func (c *recordingConn) SendDatagram(b []byte) error {
	c.sent = append(c.sent, append([]byte(nil), b...))
	return nil
}

func (c *recordingConn) ReceiveDatagram(context.Context) ([]byte, error) {
	if len(c.incoming) == 0 {
		return nil, io.EOF
	}
	next := c.incoming[0]
	c.incoming = c.incoming[1:]
	return []byte(next), nil
}

func TestDatagramSinkSplitsAtThePayloadBound(t *testing.T) {
	conn := &recordingConn{}
	n, err := (&datagramSink{conn: conn}).Write(make([]byte, wtDatagramPayload+1))
	if err != nil {
		t.Fatalf("write: %v", err)
	}
	if n != wtDatagramPayload+1 {
		t.Fatalf("wrote %d bytes, want %d", n, wtDatagramPayload+1)
	}
	if len(conn.sent) != 2 || len(conn.sent[0]) != wtDatagramPayload || len(conn.sent[1]) != 1 {
		t.Fatalf("datagram sizes = %v, want one full payload and a remainder", datagramSizes(conn.sent))
	}
}

type failingConn struct{ recordingConn }

func (c *failingConn) SendDatagram([]byte) error { return io.ErrClosedPipe }

// The flood loop in HandleSession re-runs the download until the session dies;
// the latched failure is what ends it without spinning on a dead sink.
func TestDatagramSinkLatchesASendFailure(t *testing.T) {
	sink := &datagramSink{conn: &failingConn{}}
	if _, err := sink.Write(make([]byte, 1)); err == nil {
		t.Fatal("write on a dead sink reported no error")
	}
	if !sink.failed {
		t.Fatal("send failure did not latch")
	}
}

// Every spelling of zero is the park path. A zero that reached the lane loop
// would spin without moving bytes for the whole session lifetime.
func TestWTDownloadParksOnEveryZeroSpelling(t *testing.T) {
	for _, spelling := range []string{"0", "00", "+0", "-0"} {
		if got := parseBytes(spelling); got != 0 {
			t.Errorf("parseBytes(%q) = %d, want 0 so the session parks", spelling, got)
		}
	}
	if got := parseBytes(""); got != defaultBytes {
		t.Errorf("parseBytes(\"\") = %d, want the default: an absent size is not a park request", got)
	}
}

func TestDatagramSourceYieldsOneDatagramPerRead(t *testing.T) {
	src := datagramSource{conn: &recordingConn{incoming: []string{"first", "second"}}, ctx: context.Background()}
	buf := make([]byte, 64)
	for _, want := range []string{"first", "second"} {
		n, err := src.Read(buf)
		if err != nil {
			t.Fatalf("read: %v", err)
		}
		if got := string(buf[:n]); got != want {
			t.Fatalf("read = %q, want %q", got, want)
		}
	}
	if _, err := src.Read(buf); err != io.EOF {
		t.Fatalf("read after drain = %v, want EOF", err)
	}
}

// The drain feeds the upload counter, so a datagram larger than the buffer must
// refuse rather than deliver a prefix and undercount the rest.
func TestDatagramSourceRefusesToTruncate(t *testing.T) {
	src := datagramSource{conn: &recordingConn{incoming: []string{"a datagram longer than the buffer"}}, ctx: context.Background()}
	if _, err := src.Read(make([]byte, 8)); err != io.ErrShortBuffer {
		t.Fatalf("read into a short buffer = %v, want io.ErrShortBuffer", err)
	}
}

// TestUploadProgressHandleStreamReportsTheCounter runs the WebTransport feed
// over a pipe: ready first, then the terminal count once the upload finishes.
func TestUploadProgressHandleStreamReportsTheCounter(t *testing.T) {
	store := NewUploadStore()
	id := store.Mint()
	agg, access := store.getOrCreateFor(id, "owner")
	if access != uploadAccessOK {
		t.Fatalf("getOrCreateFor = %v, want ok", access)
	}
	agg.recordChunk(monoNanos(), 4096)

	r, w := io.Pipe()
	go func() {
		NewUploadProgress(store).HandleStream(context.Background(), id, "owner", w)
		_ = w.Close()
	}()

	records := bufio.NewScanner(r)
	if got := nextProgressEvent(t, records).Type; got != "ready" {
		t.Fatalf("first record = %q, want ready", got)
	}
	time.AfterFunc(50*time.Millisecond, func() { store.finishFor(id, "owner") })
	for {
		event := nextProgressEvent(t, records)
		if event.Type == "progress" {
			continue
		}
		if event.Type != "complete" || event.Bytes != 4096 {
			t.Fatalf("terminal record = %+v, want complete with 4096 bytes", event)
		}
		return
	}
}

// TestUploadProgressHandleStreamRefusesAnUnknownID reports the refusal as a
// record, since a stream has no status code.
func TestUploadProgressHandleStreamRefusesAnUnknownID(t *testing.T) {
	var out strings.Builder
	NewUploadProgress(NewUploadStore()).HandleStream(context.Background(), "gmu_missing", "owner", &out)

	var event uploadProgressEvent
	if err := json.Unmarshal([]byte(out.String()), &event); err != nil {
		t.Fatalf("decode %q: %v", out.String(), err)
	}
	if event.Type != "error" || event.Message != uploadAccessMessage(uploadAccessInvalid) {
		t.Fatalf("record = %+v, want the unknown-id refusal", event)
	}
}

func nextProgressEvent(t *testing.T, records *bufio.Scanner) uploadProgressEvent {
	t.Helper()
	for records.Scan() {
		if strings.TrimSpace(records.Text()) == "" {
			continue
		}
		var event uploadProgressEvent
		if err := json.Unmarshal(records.Bytes(), &event); err != nil {
			t.Fatalf("decode %q: %v", records.Text(), err)
		}
		return event
	}
	t.Fatal("progress stream ended early")
	return uploadProgressEvent{}
}

func datagramSizes(sent [][]byte) []int {
	sizes := make([]int, len(sent))
	for i, d := range sent {
		sizes[i] = len(d)
	}
	return sizes
}
