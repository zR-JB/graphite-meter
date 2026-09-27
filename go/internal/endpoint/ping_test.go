package endpoint

import (
	"io"
	"testing"
	"testing/synctest"
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

// A pong reports the time from recv's return to its encoding, never the recv wait.
func TestServePingReportsHandlingAfterRecv(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		left := 3
		ServePing(func() ([]byte, error) {
			if left == 0 {
				return nil, io.EOF
			}
			left--
			time.Sleep(time.Second)
			return []byte(wire.EncodePing(uint32(left))), nil
		}, func(reply []byte) error {
			pong, err := wire.DecodePong(string(reply))
			if err != nil || pong.ID != uint32(left) || pong.HandlingNanos != 0 {
				t.Fatalf("reply %q: %v, want the id and no time from the recv wait", reply, err)
			}
			return nil
		})
	})
}
