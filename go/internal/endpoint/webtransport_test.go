package endpoint

import (
	"bytes"
	"context"
	"errors"
	"io"
	"net"
	"net/url"
	"strconv"
	"testing"
	"testing/synctest"
	"time"

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
		ctx, live := WatchIdle(t.Context(), time.Second)
		for range 10 {
			time.Sleep(400 * time.Millisecond)
			live.Bump()
		}
		synctest.Wait()
		if ctx.Err() != nil {
			t.Fatal("an active session was ended")
		}
		time.Sleep(2 * time.Second)
		synctest.Wait()
		if !errors.Is(context.Cause(ctx), errIdle) {
			t.Fatalf("a session quiet for two bounds ended with %v", context.Cause(ctx))
		}
	})
}

// A peer draining one block slower than the idle bound keeps its session, since every piece counts as activity.
func TestSlowlyDrainedLaneStaysActive(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		const bound = time.Second
		ctx, live := WatchIdle(t.Context(), bound)
		client, server := net.Pipe()
		defer client.Close()
		lane := &idleWriter{w: server, idle: idleDeadline{bound: bound, live: live}}
		go func() {
			_, _ = lane.Write(make([]byte, 256<<10))
			server.Close()
		}()
		piece := make([]byte, 16<<10)
		for i := range 16 {
			time.Sleep(bound * 2 / 5)
			if _, err := io.ReadFull(client, piece); err != nil || ctx.Err() != nil {
				t.Fatalf("piece %d after %v: %v, session %v", i, time.Duration(i+1)*bound*2/5, err, ctx.Err())
			}
		}
	})
}

type countingWriter struct{ writes int }

func (c *countingWriter) Write(p []byte) (int, error) {
	c.writes++
	return len(p), nil
}

// Every write costs a syscall or a frame handoff, so a peer that drains quickly takes whole blocks.
func TestQuicklyDrainedLaneTakesWholeBlocks(t *testing.T) {
	sink := &countingWriter{}
	lane := &idleWriter{w: sink, idle: idleDeadline{bound: wire.IdleBound}}
	for range 16 {
		_, _ = lane.Write(make([]byte, 256<<10))
	}
	if sink.writes > 20 {
		t.Fatalf("16 blocks took %d writes, want the pieces grown to whole blocks after the first", sink.writes)
	}
}

// Queries clamp or fall back, never reject (api/wire.md); a spelling of zero is a verify session or no datagrams.
func TestQueryParametersClampOrFallBack(t *testing.T) {
	for _, tc := range []struct {
		query     string
		bytes     int64
		streams   int
		datagrams bool
	}{
		{"", defaultBytes, 1, false},
		{"bytes=not-a-number&streams=nonsense&datagrams=nonsense", defaultBytes, 1, true},
		{"bytes=-5&streams=-2&datagrams=", defaultBytes, 1, true},
		{"bytes=" + strconv.FormatInt(maxBytes+1, 10) + "&streams=99&datagrams=1", maxBytes, 16, true},
		{"bytes=1024&streams=3", 1024, 3, false},
		{"bytes=0&streams=0&datagrams=0", 0, 1, false},
		{"bytes=00&streams=16&datagrams=00", 0, 16, false},
		{"bytes=%2B0&datagrams=+0", 0, 1, false},
		{"bytes=-0&datagrams=-0", 0, 1, false},
		{"datagrams=2", defaultBytes, 1, true},
		{"datagrams=false", defaultBytes, 1, false},
		{"datagrams=off", defaultBytes, 1, false},
		{"datagrams=no", defaultBytes, 1, false},
	} {
		query, err := url.ParseQuery(tc.query)
		if err != nil {
			t.Fatalf("parse %q: %v", tc.query, err)
		}
		n, streams, datagrams := parseBytes(query.Get("bytes")), wtStreamCount(query), wtDatagramMode(query)
		if n != tc.bytes || streams != tc.streams || datagrams != tc.datagrams {
			t.Errorf("%q = %d bytes, %d streams, datagrams %v; want %d, %d, %v", tc.query, n, streams, datagrams,
				tc.bytes, tc.streams, tc.datagrams)
		}
	}
}

// A lane is bounded by inactivity, not by one absolute deadline, and re-arming is paced rather than per chunk.
func TestIdleDeadlineReArmsWithTheClock(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		var deadlines []time.Time
		limit := time.Now().Add(wire.IdleBound + 10*time.Second)
		set := func(d time.Time) error {
			deadlines = append(deadlines, d)
			return nil
		}
		idle := &idleDeadline{set: set, bound: wire.IdleBound, limit: limit}
		for range 3 {
			idle.moved(time.Now())
		}
		if len(deadlines) != 1 {
			t.Fatalf("armed %d deadlines over three back-to-back chunks, want 1", len(deadlines))
		}
		time.Sleep(wire.IdleBound / 4)
		idle.moved(time.Now())
		if len(deadlines) != 2 || !deadlines[1].Equal(time.Now().Add(wire.IdleBound)) {
			t.Fatalf("deadlines = %v, want a second one a full bound after the later chunk", deadlines)
		}
		time.Sleep(wire.IdleBound / 2)
		idle.moved(time.Now())
		if len(deadlines) != 3 || !deadlines[2].Equal(limit) {
			t.Fatalf("deadlines = %v, want the last capped at the lane's lifetime", deadlines)
		}
	})
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
