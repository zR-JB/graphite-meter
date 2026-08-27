package wire

import (
	"strconv"
	"strings"
)

// Frame is a parsed message-bus frame (api/wire.md).
type Frame struct {
	Op string // one of the Op* keyword constants (opcodes.go)
	ID uint32 // PING / PONG: client-owned monotonic id, echoed verbatim
	// Nanos is PONG's server monotonic timestamp for diagnostics/skew only.
	Nanos uint64
	Proto string // HI: "ws" | "wt"
	Code  string // ERR: short error token
	Text  string // ERR: human detail
}

// timeField is the keyword that prefixes the nanos arg inside a PONG frame: PONG,<id>;TIME,<nanos>.
const timeField = "TIME"

// Decode error codes: the <code> token a receiver echoes back as ERR,<code>,… when it rejects a frame.
const (
	ErrBadOp   = "bad_op"   // unknown opcode keyword
	ErrBadArgs = "bad_args" // opcode known, args missing/malformed
)

// DecodeError is returned by Decode for a malformed frame.
type DecodeError struct {
	Code string
	Text string
}

func (e *DecodeError) Error() string { return e.Code + ": " + e.Text }

func badOp(text string) error   { return &DecodeError{Code: ErrBadOp, Text: text} }
func badArgs(text string) error { return &DecodeError{Code: ErrBadArgs, Text: text} }

// Decode parses one on-wire message into a Frame by slicing on ','.
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

	case OpHI:
		if rest == "" {
			return Frame{}, badArgs("HI proto")
		}
		return Frame{Op: OpHI, Proto: rest}, nil

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

// Encode renders a Frame to its exact on-wire string.
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
	case OpHI:
		return OpHI + "," + f.Proto
	case OpERR:
		return OpERR + "," + f.Code + "," + f.Text
	default:
		return ""
	}
}

// cut splits s at the first occurrence of sep into (before, after).
func cut(s string, sep byte) (before, after string) {
	if before, after, found := strings.Cut(s, string(sep)); found {
		return before, after
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
