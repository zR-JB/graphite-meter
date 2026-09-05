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
	// Timing negotiates optional application reflector timing on HI/READY.
	Timing bool
	// HandlingNanos is present only on an opted-in PONG; zero is a valid duration.
	HandlingNanos *uint64
	Proto         string // HI: "ws" | "wt"
	Code          string // ERR: short error token
	Text          string // ERR: human detail
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
	op, rest, _ := strings.Cut(msg, ",")

	switch op {
	case OpREADY:
		return Frame{Op: OpREADY, Timing: rest == "TIMING,1"}, nil
	case OpBYE:
		return Frame{Op: OpBYE}, nil

	case OpPING:
		id, err := strconv.ParseUint(rest, 10, 32)
		if err != nil {
			return Frame{}, badArgs("PING id")
		}
		return Frame{Op: OpPING, ID: uint32(id)}, nil

	case OpPONG:
		// rest = "<id>;TIME,<nanos>"
		idStr, tail, _ := strings.Cut(rest, ";")
		id, err := strconv.ParseUint(idStr, 10, 32)
		if err != nil {
			return Frame{}, badArgs("PONG id")
		}
		key, nanosStr, _ := strings.Cut(tail, ",")
		if key != timeField {
			return Frame{}, badArgs("PONG TIME")
		}
		nanosStr, handling, hasHandling := strings.Cut(nanosStr, ";")
		nanos, err := strconv.ParseUint(nanosStr, 10, 64)
		if err != nil {
			return Frame{}, badArgs("PONG nanos")
		}
		f := Frame{Op: OpPONG, ID: uint32(id), Nanos: nanos}
		if hasHandling {
			value, ok := strings.CutPrefix(handling, "HANDLING,")
			if ok {
				// Optional diagnostic corruption must not invalidate a base echo.
				if duration, err := strconv.ParseUint(value, 10, 64); err == nil {
					f.HandlingNanos = new(duration)
				}
			}
		}
		return f, nil

	case OpHI:
		if rest == "" {
			return Frame{}, badArgs("HI proto")
		}
		proto, timing := strings.CutSuffix(rest, ";TIMING,1")
		return Frame{Op: OpHI, Proto: proto, Timing: timing}, nil

	case OpERR:
		code, text, _ := strings.Cut(rest, ",")
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
		if f.Timing {
			return OpREADY + ",TIMING,1"
		}
		return OpREADY
	case OpBYE:
		return OpBYE
	case OpPING:
		return OpPING + "," + strconv.FormatUint(uint64(f.ID), 10)
	case OpPONG:
		message := OpPONG + "," + strconv.FormatUint(uint64(f.ID), 10) + ";" + timeField + "," + strconv.FormatUint(f.Nanos, 10)
		if f.HandlingNanos != nil {
			message += ";HANDLING," + strconv.FormatUint(*f.HandlingNanos, 10)
		}
		return message
	case OpHI:
		if f.Timing {
			return OpHI + "," + f.Proto + ";TIMING,1"
		}
		return OpHI + "," + f.Proto
	case OpERR:
		return OpERR + "," + f.Code + "," + f.Text
	default:
		return ""
	}
}
