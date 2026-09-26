package endpoint

import (
	"bufio"
	"bytes"
	"context"
	"encoding/json/v2"
	"errors"
	"io"
	"net/http"
	"net/http/httptest"
	"net/url"
	"strings"
	"testing"
	"testing/synctest"
	"time"

	"github.com/zR-JB/graphite-meter/go/internal/auth"
	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

// recordingConn is the datagram half of a session: it replays queued datagrams and records what was sent.
type recordingConn struct {
	incoming []string
	sent     [][]byte
}

func (c *recordingConn) SendDatagram(b []byte) error {
	c.sent = append(c.sent, bytes.Clone(b))
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

type failingConn struct{ recordingConn }

func (c *failingConn) SendDatagram([]byte) error { return io.ErrClosedPipe }

// SendDatagram ignores cancellation, so an ended session must stop the sink between datagrams.
func TestDatagramSink(t *testing.T) {
	conn := &recordingConn{}
	n, err := (&datagramSink{conn: conn}).Write(make([]byte, wtDatagramPayload+1))
	if err != nil || n != wtDatagramPayload+1 || len(conn.sent) != 2 || len(conn.sent[0]) != wtDatagramPayload {
		t.Fatalf("write = %d, %v in %d datagrams, want one full payload and a remainder", n, err, len(conn.sent))
	}
	ended := make(chan struct{})
	close(ended)
	for name, sink := range map[string]*datagramSink{
		"failed send":   {conn: &failingConn{}},
		"ended session": {conn: conn, done: ended},
	} {
		sent := len(conn.sent)
		if n, err := sink.Write(make([]byte, 4*wtDatagramPayload)); err == nil || n != 0 || !sink.failed ||
			len(conn.sent) != sent {
			t.Errorf("%s: write = %d, %v, want nothing sent and a latched failure", name, n, err)
		}
	}
}

// A session ends after two quiet half-bounds, never while its peer keeps it active.
func TestSessionWatcherEndsOnlyQuietSessions(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		ctx, live := watchSession(t.Context(), time.Second)
		for range 10 {
			time.Sleep(400 * time.Millisecond)
			live.bump()
		}
		synctest.Wait()
		if ctx.Err() != nil {
			t.Fatal("an active session was ended")
		}
		time.Sleep(2 * time.Second)
		synctest.Wait()
		if ctx.Err() == nil {
			t.Fatal("a session quiet for two bounds is still open")
		}
	})
}

// ?datagrams= is presence-based, but a spelling of zero is a refusal rather than presence.
func TestWTDatagramModeParsesRatherThanComparingSpellings(t *testing.T) {
	for _, tc := range []struct {
		query string
		want  bool
	}{
		{"", false},
		{"bytes=1024&streams=2", false},
		{"datagrams=", true},
		{"datagrams=1", true},
		{"datagrams=2", true},
		{"datagrams=nonsense", true},
		{"datagrams=0", false},
		{"datagrams=00", false},
		{"datagrams=+0", false},
		{"datagrams=-0", false},
		{"datagrams=false", false},
		{"datagrams=off", false},
		{"datagrams=no", false},
	} {
		query, err := url.ParseQuery(tc.query)
		if err != nil {
			t.Fatalf("parse %q: %v", tc.query, err)
		}
		if got := wtDatagramMode(query); got != tc.want {
			t.Errorf("wtDatagramMode(%q) = %v, want %v", tc.query, got, tc.want)
		}
	}
}

// deadlineRecordingStream records every read deadline armed on it.
type deadlineRecordingStream struct{ deadlines []time.Time }

func (s *deadlineRecordingStream) SetReadDeadline(t time.Time) error {
	s.deadlines = append(s.deadlines, t)
	return nil
}

func (s *deadlineRecordingStream) Read(p []byte) (int, error) { return len(p), nil }

// A lane is bounded by inactivity, not by one absolute deadline, and re-arming is paced rather than per read.
func TestIdleTimeoutReaderReArmsItsDeadlineWithTheClock(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		stream := &deadlineRecordingStream{}
		reader := &idleTimeoutReader{str: stream, timeout: 8 * time.Second}
		buf := make([]byte, 8)
		for range 3 {
			_, _ = reader.Read(buf)
		}
		if len(stream.deadlines) != 1 {
			t.Fatalf("armed %d deadlines over three back-to-back reads, want 1", len(stream.deadlines))
		}
		time.Sleep(2 * time.Second)
		_, _ = reader.Read(buf)
		if len(stream.deadlines) != 2 || !stream.deadlines[1].Equal(time.Now().Add(8*time.Second)) {
			t.Fatalf("deadlines = %v, want a second one a full timeout after the later read", stream.deadlines)
		}
	})
}

// A capped mint is retryable and a refused one is not; neither is an authentication challenge.
func TestSocketToken(t *testing.T) {
	for _, tc := range []struct {
		name              string
		mint              SocketTokenMinter
		status            int
		retryAfter, token string
		expires           int64
	}{
		{"public mode", nil, http.StatusOK, "", "", 0},
		{"minted", func(*http.Request) (string, time.Time, auth.WTMint) {
			return "gmw_minted", time.UnixMilli(3_600_000), auth.WTMintOK
		}, http.StatusOK, "", "gmw_minted", 3_600_000},
		{"at capacity", func(*http.Request) (string, time.Time, auth.WTMint) {
			return "", time.Time{}, auth.WTMintAtCapacity
		}, http.StatusTooManyRequests, "1", "", 0},
		{"no session", func(*http.Request) (string, time.Time, auth.WTMint) {
			return "", time.Time{}, auth.WTMintNoSession
		}, http.StatusForbidden, "", "", 0},
	} {
		t.Run(tc.name, func(t *testing.T) {
			rec := httptest.NewRecorder()
			SocketToken(tc.mint).ServeHTTP(rec, httptest.NewRequest(http.MethodPost, "/wt/session", nil))
			if rec.Code != tc.status || rec.Header().Get("Retry-After") != tc.retryAfter ||
				rec.Header().Get("Graphite-Meter-Auth") != "" {
				t.Fatalf("status = %d headers = %v", rec.Code, rec.Header())
			}
			var body struct {
				Token   string `json:"token"`
				Expires int64  `json:"expires"`
			}
			if tc.status == http.StatusOK && (json.Unmarshal(rec.Body.Bytes(), &body) != nil ||
				body.Token != tc.token || body.Expires != tc.expires) {
				t.Fatalf("body = %s", rec.Body.String())
			}
		})
	}
}

// The drain feeds the upload counter, so a datagram larger than the buffer must refuse rather than deliver a prefix.
func TestDatagramSourceYieldsWholeDatagrams(t *testing.T) {
	src := datagramSource{conn: &recordingConn{incoming: []string{"first", "second", "longer than eight"}},
		ctx: t.Context()}
	buf := make([]byte, 8)
	for _, want := range []string{"first", "second"} {
		if n, err := src.Read(buf); err != nil || string(buf[:n]) != want {
			t.Fatalf("read = %q, %v, want %q", buf[:n], err, want)
		}
	}
	if _, err := src.Read(buf); err != io.ErrShortBuffer {
		t.Fatalf("read into a short buffer = %v, want io.ErrShortBuffer", err)
	}
	if _, err := src.Read(buf); !errors.Is(err, io.EOF) {
		t.Fatalf("read after drain = %v, want EOF", err)
	}
}

func TestStreamProgressReportsTheCounter(t *testing.T) {
	store := NewUpload(nil, nil)
	id := store.Mint()
	agg, access := store.getOrCreateFor(id, "owner")
	if access != uploadAccessOK {
		t.Fatalf("getOrCreateFor = %v, want ok", access)
	}
	agg.recordChunk(store.now(), 4096)

	r, w := io.Pipe()
	go func() {
		store.streamProgress(t.Context(), agg, w)
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

func nextProgressEvent(t *testing.T, records *bufio.Scanner) wire.UploadProgress {
	t.Helper()
	for records.Scan() {
		if strings.TrimSpace(records.Text()) == "" {
			continue
		}
		var event wire.UploadProgress
		if err := json.Unmarshal(records.Bytes(), &event); err != nil {
			t.Fatalf("decode %q: %v", records.Text(), err)
		}
		return event
	}
	t.Fatal("progress stream ended early")
	return wire.UploadProgress{}
}
