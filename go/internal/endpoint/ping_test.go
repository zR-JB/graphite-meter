package endpoint

import (
	"io"
	"testing"
	"time"

	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

// The reflector path alone, without a socket or scheduler dominating it; it should allocate nothing per probe.
func BenchmarkServePing(b *testing.B) {
	probe := []byte("PING,4294967295")
	var replyBytes int
	b.ReportAllocs()
	for b.Loop() {
		left := 1_000
		ServePing(func() ([]byte, error) {
			if left == 0 {
				return nil, io.EOF
			}
			left--
			return probe, nil
		}, func(reply []byte) error {
			replyBytes += len(reply)
			return nil
		})
	}
	b.ReportMetric(float64(b.Elapsed().Nanoseconds())/float64(b.N*1_000), "ns/probe")
	b.ReportMetric(float64(replyBytes)/float64(b.N*1_000), "reply-bytes/probe")
}

// A pong reports the time from recv's return to its encoding: never the recv wait, and not always zero.
func TestServePingReportsHandlingAfterRecv(t *testing.T) {
	var returned time.Time
	left, positive := 20, false
	ServePing(func() ([]byte, error) {
		if left == 0 {
			return nil, io.EOF
		}
		left--
		time.Sleep(time.Millisecond)
		returned = time.Now()
		return []byte(wire.EncodePing(uint32(left))), nil
	}, func(reply []byte) error {
		window := time.Since(returned)
		pong, err := wire.DecodePong(string(reply))
		if err != nil || pong.ID != uint32(left) {
			t.Fatalf("reply %q: %v", reply, err)
		}
		if handling := time.Duration(pong.HandlingNanos); handling > window {
			t.Fatalf("handling %v exceeds the %v since recv returned", handling, window)
		}
		positive = positive || pong.HandlingNanos > 0
		return nil
	})
	if !positive {
		t.Fatal("every pong reported zero handling time")
	}
}
