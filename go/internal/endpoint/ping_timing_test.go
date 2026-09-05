package endpoint

import (
	"io"
	"testing"
	"time"

	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

type pingScriptBus struct {
	messages []string
	replies  []string
}

func (b *pingScriptBus) Recv() (string, error) {
	if len(b.messages) == 0 {
		return "", io.EOF
	}
	message := b.messages[0]
	b.messages = b.messages[1:]
	return message, nil
}

func (b *pingScriptBus) Send(message string) error {
	b.replies = append(b.replies, message)
	return nil
}

func TestEveryPingReplyIncludesHandlingTime(t *testing.T) {
	bus := &pingScriptBus{messages: []string{"PING,0", "PING,1", "PING,1"}}
	started := time.Now()
	if err := NewPing().HandleMessages(t.Context(), bus); err != nil {
		t.Fatal(err)
	}
	elapsed := uint64(time.Since(started).Nanoseconds()) //nosec G115 -- monotonic elapsed duration
	if len(bus.replies) != 3 {
		t.Fatalf("replies = %v", bus.replies)
	}
	for index, message := range bus.replies {
		pong, err := wire.DecodePong(message)
		if err != nil || pong.HandlingNanos > elapsed {
			t.Fatalf("reply %d = %q: %v", index, message, err)
		}
	}
}

// A finite bus measures the real endpoint path without a socket or scheduler dominating it.
type benchmarkPingBus struct {
	left  int
	bytes int
}

func (b *benchmarkPingBus) Recv() (string, error) {
	if b.left == 0 {
		return "", io.EOF
	}
	b.left--
	return "PING,1", nil
}

func (b *benchmarkPingBus) Send(message string) error {
	b.bytes += len(message)
	return nil
}

func BenchmarkPingReflectorTiming(b *testing.B) {
	endpoint := NewPing()
	bus := &benchmarkPingBus{}
	b.ReportAllocs()
	for b.Loop() {
		bus.left = 1_000
		if err := endpoint.HandleMessages(b.Context(), bus); err != nil {
			b.Fatal(err)
		}
	}
	b.ReportMetric(float64(b.Elapsed().Nanoseconds())/float64(b.N*1_000), "ns/probe")
	b.ReportMetric(float64(bus.bytes)/float64(b.N*1_000), "reply-bytes/probe")
}
