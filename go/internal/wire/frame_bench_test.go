package wire

import "testing"

// The message-bus decoder is on the ping hot path: one Decode per PING/PONG per
// connection. These pin its allocation-free, JSON-free cost so a regression
// (e.g. switching to a reflective parser) shows up.

func BenchmarkDecodePING(b *testing.B) {
	msg := Encode(Frame{Op: OpPING, ID: 4294967295})
	b.ReportAllocs()
	for b.Loop() {
		if _, err := Decode(msg); err != nil {
			b.Fatal(err)
		}
	}
}

func BenchmarkDecodePONG(b *testing.B) {
	msg := Encode(Frame{Op: OpPONG, ID: 4294967295, Nanos: 9223372036854775807})
	b.ReportAllocs()
	for b.Loop() {
		if _, err := Decode(msg); err != nil {
			b.Fatal(err)
		}
	}
}
