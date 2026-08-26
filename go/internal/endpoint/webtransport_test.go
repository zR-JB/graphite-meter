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

	"github.com/quic-go/webtransport-go"
	"github.com/zR-JB/graphite-meter/go/internal/auth"
	"github.com/zR-JB/graphite-meter/go/internal/transport"
)

// recordingConn is the datagram half of a session: it replays queued datagrams
// and records what was sent.
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

// ?datagrams= is presence-based, but a spelling of zero is a refusal rather
// than presence. HandleSession states the rule eleven lines above the mode
// switch -- "any zero request serves nothing" -- and bytes= follows it, so a
// mode switch that compared the raw spelling turned the flood and the drain on
// for ?datagrams=0.
func TestWTDatagramModeParsesRatherThanComparingSpellings(t *testing.T) {
	for _, tc := range []struct {
		query string
		want  bool
	}{
		{"", false},
		{"bytes=1024&streams=2", false},
		// Presence is the documented request, whatever it is spelled with.
		{"datagrams=", true},
		{"datagrams=1", true},
		{"datagrams=2", true},
		{"datagrams=nonsense", true},
		// Every zero and every refusal is a request for no datagrams.
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

// refusingLane is a lane whose peer resets it: every write is refused before a
// byte lands, which is what a lane carrying nothing looks like from the server.
type refusingLane struct{ closed bool }

func (l *refusingLane) Write([]byte) (int, error)                { return 0, io.ErrClosedPipe }
func (l *refusingLane) Close() error                             { l.closed = true; return nil }
func (l *refusingLane) CancelWrite(webtransport.StreamErrorCode) {}
func (l *refusingLane) SetWriteDeadline(time.Time) error         { return nil }

// countingLanes counts how many lanes the loop asked for. The limit is a
// harness bound, not a server one: without the anti-churn guard the loop never
// stops on its own, and a test that hangs reports nothing.
type countingLanes struct {
	opened int
	limit  int
}

func (l *countingLanes) openLane(context.Context) (laneStream, error) {
	if l.opened >= l.limit {
		return nil, io.ErrUnexpectedEOF
	}
	l.opened++
	return &refusingLane{}, nil
}

// A download lane is replaced the moment it is exhausted, for as long as the
// session lives. A peer that resets everything it is handed would therefore have
// the server opening streams at handshake speed for the whole session bound,
// paying a stream setup per refusal. A lane that carried nothing is that peer,
// and the loop stops rather than reopening.
func TestServeLaneStopsOnceAPeerRefusesALane(t *testing.T) {
	lanes := &countingLanes{limit: 64}
	h := &wtDownload{download: NewDownload(make([]byte, 4096), nil)}

	h.serveLane(t.Context(), lanes, url.Values{"bytes": {"4096"}}, nil)

	if lanes.opened != 1 {
		t.Fatalf("opened %d lanes against a peer that refused every one, want 1: the loop reopens streams for as long as the peer keeps refusing them", lanes.opened)
	}
}

// deadlineRecordingStream records the read deadline armed before each read and
// advances a clock, so a re-armed bound is distinguishable from one absolute
// deadline set once.
type deadlineRecordingStream struct{ deadlines []time.Time }

func (s *deadlineRecordingStream) SetReadDeadline(t time.Time) error {
	s.deadlines = append(s.deadlines, t)
	return nil
}

func (s *deadlineRecordingStream) Read(p []byte) (int, error) {
	// Real elapsed time, so a re-armed deadline is strictly later than the last.
	time.Sleep(time.Millisecond)
	return len(p), nil
}

// A lane is bounded by inactivity, not by one absolute deadline: the deadline
// is re-armed before every read, matching the fetch path's per-POST bound.
// Armed once instead, a lane delivering steadily would still be cut
// uploadReadTimeout after its first byte.
func TestIdleTimeoutReaderReArmsItsDeadlineEveryRead(t *testing.T) {
	stream := &deadlineRecordingStream{}
	reader := idleTimeoutReader{str: stream, timeout: time.Hour}

	buf := make([]byte, 8)
	const reads = 3
	for i := range reads {
		if _, err := reader.Read(buf); err != nil {
			t.Fatalf("read %d: %v", i, err)
		}
	}

	if len(stream.deadlines) != reads {
		t.Fatalf("armed %d deadlines over %d reads, want one per read: the lane is bounded by inactivity, not by a single deadline", len(stream.deadlines), reads)
	}
	for i := 1; i < len(stream.deadlines); i++ {
		if !stream.deadlines[i].After(stream.deadlines[i-1]) {
			t.Errorf("deadline %d (%v) did not advance past deadline %d (%v)", i, stream.deadlines[i], i-1, stream.deadlines[i-1])
		}
	}
}

// A mint refusal is two different answers. A session at its per-session token
// cap is intact and gets a slot back within the token lifetime, so the caller's
// one correct move is to wait and ask again -- capacity, which this codebase
// answers with a retryable status and Retry-After (upload_owner.go's
// writeUploadAccessError, and requestAdmission.acquire). The cap is per session,
// so it takes the per-client status those two use rather than the server-wide
// one. A request with nothing to bind a token to is the only real refusal.
func TestWTSessionSeparatesACappedMintFromARefusedOne(t *testing.T) {
	for _, tc := range []struct {
		name       string
		mint       auth.WTMint
		status     int
		retryAfter string
	}{
		{"at capacity", auth.WTMintAtCapacity, http.StatusTooManyRequests, "1"},
		{"no session", auth.WTMintNoSession, http.StatusForbidden, ""},
	} {
		t.Run(tc.name, func(t *testing.T) {
			mint := tc.mint
			endpoint := NewWTSession(func(*http.Request) (string, time.Time, auth.WTMint) {
				return "", time.Time{}, mint
			})
			rec := httptest.NewRecorder()
			if err := endpoint.Handle(transport.NewHTTPSession(rec, httptest.NewRequest(http.MethodPost, "/wt/session", nil))); err != nil {
				t.Fatalf("handle: %v", err)
			}
			if rec.Code != tc.status {
				t.Errorf("status = %d, want %d", rec.Code, tc.status)
			}
			if got := rec.Header().Get("Retry-After"); got != tc.retryAfter {
				t.Errorf("Retry-After = %q, want %q", got, tc.retryAfter)
			}
			// A refusal that carried the auth marker would send the user to a
			// login; neither of these is an authentication failure.
			if got := rec.Header().Get("Graphite-Meter-Auth"); got != "" {
				t.Errorf("Graphite-Meter-Auth = %q, want it absent", got)
			}
		})
	}

	// The control: a mint that succeeded still answers with its token.
	endpoint := NewWTSession(func(*http.Request) (string, time.Time, auth.WTMint) {
		return "gmw_minted", time.Unix(0, 0).Add(time.Hour), auth.WTMintOK
	})
	rec := httptest.NewRecorder()
	if err := endpoint.Handle(transport.NewHTTPSession(rec, httptest.NewRequest(http.MethodPost, "/wt/session", nil))); err != nil {
		t.Fatalf("handle: %v", err)
	}
	var minted wtSessionResponse
	if err := json.Unmarshal(rec.Body.Bytes(), &minted); err != nil {
		t.Fatalf("decode %q: %v", rec.Body.String(), err)
	}
	if minted.Token != "gmw_minted" {
		t.Errorf("token = %q, want the minted one", minted.Token)
	}
}

func TestDatagramSourceYieldsOneDatagramPerRead(t *testing.T) {
	src := datagramSource{conn: &recordingConn{incoming: []string{"first", "second"}}, ctx: t.Context()}
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
	if _, err := src.Read(buf); !errors.Is(err, io.EOF) {
		t.Fatalf("read after drain = %v, want EOF", err)
	}
}

// The drain feeds the upload counter, so a datagram larger than the buffer must
// refuse rather than deliver a prefix and undercount the rest.
func TestDatagramSourceRefusesToTruncate(t *testing.T) {
	src := datagramSource{conn: &recordingConn{incoming: []string{"a datagram longer than the buffer"}}, ctx: t.Context()}
	if _, err := src.Read(make([]byte, 8)); err != io.ErrShortBuffer {
		t.Fatalf("read into a short buffer = %v, want io.ErrShortBuffer", err)
	}
}

// A datagram drain refused before its first read never reaches Read, which
// used to be the only place the idle timer was stopped, so each refused session
// left a timer armed for the whole read bound.
func TestIdleTimeoutSourceDisarmsWhenTheDrainEnds(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		ctx, cancel := context.WithCancel(t.Context())
		// Long enough that a still-armed timer cannot be mistaken for a fired one.
		src := newIdleTimeoutSource(ctx, &recordingConn{}, time.Hour, nil)
		cancel()
		synctest.Wait()
		// Stop reports true only for a timer it had to stop itself.
		if src.timer.Stop() {
			t.Fatal("the idle timer was still armed after the drain ended")
		}
	})
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
		NewUploadProgress(store).HandleStream(t.Context(), id, "owner", w)
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
	NewUploadProgress(NewUploadStore()).HandleStream(t.Context(), "gmu_missing", "owner", &out)

	var event uploadProgressEvent
	if err := json.Unmarshal([]byte(out.String()), &event); err != nil {
		t.Fatalf("decode %q: %v", out.String(), err)
	}
	if event.Type != "error" || event.Message != uploadAccessMessage(uploadAccessInvalid) || event.Code != uploadAccessCode(uploadAccessInvalid) {
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
