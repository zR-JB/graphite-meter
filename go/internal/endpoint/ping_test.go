package endpoint

import (
	"io"
	"testing"
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
