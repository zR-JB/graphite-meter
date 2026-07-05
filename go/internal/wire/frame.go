package wire

import (
	"strconv"
	"strings"
)

// Frame is a parsed message-bus frame (api/wire.md). It is a flat union: only
// the fields relevant to Op are meaningful. Decode/Encode are byte-exact against
// the shared corpus api/wire.testvectors.txt — the cross-language contract.
type Frame struct {
	Op string // one of the Op* keyword constants (opcodes.go)
	ID uint32 // PING / PONG — client-owned monotonic id, echoed verbatim
	// Nanos is the TIME sub-field; meaning depends on Op:
	//   • PONG: server's raw monotonic clock (ns), for the RTT echo.
	//   • BYTES_RECEIVED / UPLOAD_COMPLETE: server's ACTIVE measurement clock (ns
	//     bytes were actually flowing for this id, dead zones excluded), sampled
	//     alongside N. The client derives upload rate as Δn/Δnanos over this
	//     clock — never its own arrival clock — so stalls/reconnects can't skew
	//     it (go/internal/endpoint/upload_store.go: activeNanos).
	Nanos uint64
	Bytes uint64 // SIZE — requested byte count
	N     uint64 // BYTES_RECEIVED / UPLOAD_COMPLETE — server-measured byte total
	Proto string // HI — "ws" | "wt"
	Code  string // ERR — short error token
	Text  string // ERR — human detail
}

// timeField is the keyword that prefixes the nanos arg inside a PONG frame:
// PONG,<id>;TIME,<nanos>. It is a sub-field of PONG, not a standalone opcode.
const timeField = "TIME"

// Decode error codes — the <code> token a receiver echoes back as ERR,<code>,…
// when it rejects a frame. Stable, cross-language.
const (
	ErrBadOp   = "bad_op"   // unknown opcode keyword
	ErrBadArgs = "bad_args" // opcode known, args missing/malformed
)

// DecodeError is returned by Decode for a malformed frame. Code is the stable
// token the ping endpoint copies into ERR,<code>,<text>; the bus is never torn
// down for a single bad frame.
type DecodeError struct {
	Code string
	Text string
}

func (e *DecodeError) Error() string { return e.Code + ": " + e.Text }

func badOp(text string) error   { return &DecodeError{Code: ErrBadOp, Text: text} }
func badArgs(text string) error { return &DecodeError{Code: ErrBadArgs, Text: text} }

// Decode parses one on-wire message into a Frame. Parsing is indexOf(',') slicing
// — never JSON, never regex. An unknown opcode yields ErrBadOp; a known opcode
// with missing/malformed args yields ErrBadArgs (both as *DecodeError).
func Decode(msg string) (Frame, error) {
	op, rest := cut(msg, ',')

	switch op {
	case OpREADY:
		return Frame{Op: OpREADY}, nil
	case OpBYE:
		return Frame{Op: OpBYE}, nil

	case OpPING:
		id, ok := parseU32(rest)
		if !ok {
			return Frame{}, badArgs("PING id")
		}
		return Frame{Op: OpPING, ID: id}, nil

	case OpPONG:
		// rest = "<id>;TIME,<nanos>"
		idStr, tail := cut(rest, ';')
		id, ok := parseU32(idStr)
		if !ok {
			return Frame{}, badArgs("PONG id")
		}
		key, nanosStr := cut(tail, ',')
		if key != timeField {
			return Frame{}, badArgs("PONG TIME")
		}
		nanos, ok := parseU64(nanosStr)
		if !ok {
			return Frame{}, badArgs("PONG nanos")
		}
		return Frame{Op: OpPONG, ID: id, Nanos: nanos}, nil

	case OpSIZE:
		bytes, ok := parseU64(rest)
		if !ok {
			return Frame{}, badArgs("SIZE bytes")
		}
		return Frame{Op: OpSIZE, Bytes: bytes}, nil

	case OpHI:
		if rest == "" {
			return Frame{}, badArgs("HI proto")
		}
		return Frame{Op: OpHI, Proto: rest}, nil

	case OpBytesReceived:
		n, nanos, err := parseCountTime(rest, "BYTES_RECEIVED")
		if err != nil {
			return Frame{}, err
		}
		return Frame{Op: OpBytesReceived, N: n, Nanos: nanos}, nil

	case OpUploadComplete:
		n, nanos, err := parseCountTime(rest, "UPLOAD_COMPLETE")
		if err != nil {
			return Frame{}, err
		}
		return Frame{Op: OpUploadComplete, N: n, Nanos: nanos}, nil

	case OpERR:
		code, text := cut(rest, ',')
		if code == "" {
			return Frame{}, badArgs("ERR code")
		}
		return Frame{Op: OpERR, Code: code, Text: text}, nil

	default:
		return Frame{}, badOp(op)
	}
}

// Encode renders a Frame to its exact on-wire string. An unknown Op yields "".
func Encode(f Frame) string {
	switch f.Op {
	case OpREADY:
		return OpREADY
	case OpBYE:
		return OpBYE
	case OpPING:
		return OpPING + "," + u32(f.ID)
	case OpPONG:
		return OpPONG + "," + u32(f.ID) + ";" + timeField + "," + u64(f.Nanos)
	case OpSIZE:
		return OpSIZE + "," + u64(f.Bytes)
	case OpHI:
		return OpHI + "," + f.Proto
	case OpBytesReceived:
		return OpBytesReceived + "," + u64(f.N) + ";" + timeField + "," + u64(f.Nanos)
	case OpUploadComplete:
		return OpUploadComplete + "," + u64(f.N) + ";" + timeField + "," + u64(f.Nanos)
	case OpERR:
		return OpERR + "," + f.Code + "," + f.Text
	default:
		return ""
	}
}

// parseCountTime parses the "<n>;TIME,<nanos>" body shared by BYTES_RECEIVED and
// UPLOAD_COMPLETE (see Frame.Nanos for what the TIME value measures). op names
// the frame for the error token.
func parseCountTime(rest, op string) (n, nanos uint64, err error) {
	nStr, tail := cut(rest, ';')
	if n, ok := parseU64(nStr); ok {
		key, nanosStr := cut(tail, ',')
		if key != timeField {
			return 0, 0, badArgs(op + " TIME")
		}
		if nanos, ok := parseU64(nanosStr); ok {
			return n, nanos, nil
		}
		return 0, 0, badArgs(op + " nanos")
	}
	return 0, 0, badArgs(op + " n")
}

// cut splits s at the first occurrence of sep into (before, after). When sep is
// absent, before is the whole string and after is empty.
func cut(s string, sep byte) (before, after string) {
	if i := strings.IndexByte(s, sep); i >= 0 {
		return s[:i], s[i+1:]
	}
	return s, ""
}

func parseU32(s string) (uint32, bool) {
	n, err := strconv.ParseUint(s, 10, 32)
	if err != nil {
		return 0, false
	}
	return uint32(n), true
}

func parseU64(s string) (uint64, bool) {
	n, err := strconv.ParseUint(s, 10, 64)
	if err != nil {
		return 0, false
	}
	return n, true
}

func u32(v uint32) string { return strconv.FormatUint(uint64(v), 10) }
func u64(v uint64) string { return strconv.FormatUint(v, 10) }
