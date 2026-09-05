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

func TestPingTimingIsOptInAndConnectionLocal(t *testing.T) {
	endpoint := NewPing()
	for _, proto := range []string{"ws", "wt"} {
		t.Run(proto, func(t *testing.T) {
			bus := &pingScriptBus{messages: []string{
				"PING,0", "HI," + proto + ";TIMING,1", "PING,1", "PING,1",
				"HI," + proto, "PING,2", "HI," + proto + ";TIMING,2", "PING,3",
			}}
			started := time.Now()
			if err := endpoint.HandleMessages(t.Context(), bus); err != nil {
				t.Fatal(err)
			}
			elapsed := uint64(time.Since(started).Nanoseconds()) //nosec G115 -- monotonic elapsed duration
			if len(bus.replies) != 8 {
				t.Fatalf("replies = %v", bus.replies)
			}
			for i, message := range bus.replies {
				frame, err := wire.Decode(message)
				if err != nil {
					t.Fatal(err)
				}
				if frame.Op == wire.OpREADY {
					if frame.Timing != (i == 1) {
						t.Fatalf("reply %d = %q; unexpected capability", i, message)
					}
					continue
				}
				if frame.Op != wire.OpPONG || (frame.HandlingNanos != nil) != (i == 2 || i == 3) {
					t.Fatalf("reply %d = %q; unexpected timing", i, message)
				}
				if frame.HandlingNanos != nil && *frame.HandlingNanos > elapsed {
					t.Fatalf("handling duration = %d ns", *frame.HandlingNanos)
				}
			}
			// Reuse the endpoint with a fresh adapter: another client's opt-in cannot leak.
			fresh := &pingScriptBus{messages: []string{"PING,9"}}
			if err := endpoint.HandleMessages(t.Context(), fresh); err != nil {
				t.Fatal(err)
			}
			frame, err := wire.Decode(fresh.replies[0])
			if err != nil || frame.HandlingNanos != nil {
				t.Fatalf("fresh bus reply = %q, %v", fresh.replies[0], err)
			}
		})
	}
}

// A finite bus measures the real endpoint path without a socket or scheduler dominating it.
type benchmarkPingBus struct {
	timing bool
	left   int
	bytes  int
}

func (b *benchmarkPingBus) Recv() (string, error) {
	if b.left == 1_001 {
		b.left--
		if b.timing {
			return "HI,ws;TIMING,1", nil
		}
		return "HI,ws", nil
	}
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
	for _, timing := range []bool{false, true} {
		name := "legacy"
		if timing {
			name = "timing"
		}
		b.Run(name, func(b *testing.B) {
			endpoint := NewPing()
			bus := &benchmarkPingBus{timing: timing}
			b.ReportAllocs()
			for b.Loop() {
				bus.left = 1_001
				if err := endpoint.HandleMessages(b.Context(), bus); err != nil {
					b.Fatal(err)
				}
			}
			b.ReportMetric(float64(b.Elapsed().Nanoseconds())/float64(b.N*1_000), "ns/probe")
			b.ReportMetric(float64(bus.bytes)/float64(b.N*1_000), "reply-bytes/probe")
		})
	}
}
