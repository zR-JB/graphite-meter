package wire

import "io"

// WebTransport byte streams carry no framing, so a stream that opens with a
// control frame terminates it with a newline (api/wire.md#framing).

// maxStreamPreamble bounds the newline-terminated frame opening a stream.
const maxStreamPreamble = 64

// EncodeStreamPreamble renders a frame as a stream preamble.
func EncodeStreamPreamble(f Frame) string { return Encode(f) + "\n" }

// ReadStreamPreamble reads one newline-terminated frame from r. Reads are
// byte-wise so the bytes after the preamble stay on the stream.
func ReadStreamPreamble(r io.Reader) (Frame, error) {
	var buf [maxStreamPreamble]byte
	var one [1]byte
	for n := range len(buf) {
		if _, err := io.ReadFull(r, one[:]); err != nil {
			return Frame{}, err
		}
		if one[0] == '\n' {
			return Decode(string(buf[:n]))
		}
		buf[n] = one[0]
	}
	return Frame{}, badArgs("preamble length")
}
