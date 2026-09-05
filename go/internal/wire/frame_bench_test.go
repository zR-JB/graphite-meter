package wire

import "testing"

func BenchmarkDecodePING(b *testing.B) {
	message := EncodePing(4294967295)
	b.ReportAllocs()
	for b.Loop() {
		if _, err := DecodePing(message); err != nil {
			b.Fatal(err)
		}
	}
}

func BenchmarkDecodePONG(b *testing.B) {
	message := EncodePong(4294967295, 9223372036854775807)
	b.ReportAllocs()
	for b.Loop() {
		if _, err := DecodePong(message); err != nil {
			b.Fatal(err)
		}
	}
}

var encodedFrame string

func BenchmarkEncodePONG(b *testing.B) {
	b.ReportAllocs()
	for b.Loop() {
		encodedFrame = EncodePong(4294967295, 9223372036854775807)
	}
}
