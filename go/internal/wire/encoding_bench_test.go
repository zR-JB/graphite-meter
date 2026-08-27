package wire

import (
	"encoding/binary"
	"encoding/json/v2"
	"testing"
)

// Why the ping bus keeps a text codec while the progress feed carries NDJSON.

type jsonPong struct {
	Op    string `json:"op"`
	ID    uint32 `json:"id"`
	Nanos uint64 `json:"nanos"`
}

var (
	textPong   = Encode(Frame{Op: OpPONG, ID: 4242424, Nanos: 1234567890123})
	jsonPongB  = []byte(`{"op":"PONG","id":4242424,"nanos":1234567890123}`)
	binPong    = binPongBytes()
	sinkFrame  Frame
	sinkString string
	sinkJSON   jsonPong
	sinkU64    uint64
)

func binPongBytes() []byte {
	b := make([]byte, 13)
	b[0] = 3 // opcode
	binary.LittleEndian.PutUint32(b[1:], 4242424)
	binary.LittleEndian.PutUint64(b[5:], 1234567890123)
	return b
}

func BenchmarkDecodeText(b *testing.B) {
	b.ReportAllocs()
	for b.Loop() {
		f, err := Decode(textPong)
		if err != nil {
			b.Fatal(err)
		}
		sinkFrame = f
	}
}

func BenchmarkDecodeJSON(b *testing.B) {
	b.ReportAllocs()
	for b.Loop() {
		var v jsonPong
		if err := json.Unmarshal(jsonPongB, &v); err != nil {
			b.Fatal(err)
		}
		sinkJSON = v
	}
}

// Hand-rolled fixed-layout binary: the best case a binary codec can reach, so it bounds what protobuf could win.
func BenchmarkDecodeBinary(b *testing.B) {
	b.ReportAllocs()
	for b.Loop() {
		f := Frame{
			Op:    OpPONG,
			ID:    binary.LittleEndian.Uint32(binPong[1:]),
			Nanos: binary.LittleEndian.Uint64(binPong[5:]),
		}
		sinkFrame = f
	}
}

func BenchmarkEncodeText(b *testing.B) {
	b.ReportAllocs()
	f := Frame{Op: OpPONG, ID: 4242424, Nanos: 1234567890123}
	for b.Loop() {
		sinkString = Encode(f)
	}
}

func BenchmarkEncodeJSON(b *testing.B) {
	b.ReportAllocs()
	v := jsonPong{Op: "PONG", ID: 4242424, Nanos: 1234567890123}
	for b.Loop() {
		out, err := json.Marshal(v)
		if err != nil {
			b.Fatal(err)
		}
		sinkU64 = uint64(len(out))
	}
}

func BenchmarkEncodeBinary(b *testing.B) {
	b.ReportAllocs()
	for b.Loop() {
		out := make([]byte, 13)
		out[0] = 3
		binary.LittleEndian.PutUint32(out[1:], 4242424)
		binary.LittleEndian.PutUint64(out[5:], 1234567890123)
		sinkU64 = uint64(len(out))
	}
}

// On-wire sizes for the same PONG: text 31B, JSON 48B, binary 13B.
func TestEncodingSizes(t *testing.T) {
	t.Logf("text=%dB json=%dB binary=%dB", len(textPong), len(jsonPongB), len(binPong))
}

// PONG carries TIME,<nanos> that neither client reads: the server writes it, both clients parse it, nobody consumes it.
func BenchmarkEncodePONGWithoutNanos(b *testing.B) {
	b.ReportAllocs()
	for b.Loop() {
		sinkString = OpPONG + "," + u32(4242424)
	}
}

func BenchmarkDecodePONGWithoutNanos(b *testing.B) {
	b.ReportAllocs()
	msg := OpPONG + "," + u32(4242424)
	for b.Loop() {
		op, rest := cut(msg, ',')
		id, ok := parseU32(rest)
		if !ok {
			b.Fatal("bad id")
		}
		sinkFrame = Frame{Op: op, ID: id}
	}
}
