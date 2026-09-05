package wire

import "testing"

// Decode sits on the ping hot path: one call per PING/PONG per connection.

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

var encodedFrame string

func BenchmarkEncodePONG(b *testing.B) {
	frame := Frame{Op: OpPONG, ID: 4294967295, Nanos: 9223372036854775807}
	b.ReportAllocs()
	for b.Loop() {
		encodedFrame = Encode(frame)
	}
}
