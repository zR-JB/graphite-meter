package wire

// Opcode keyword table, the Go half of the cross-language pin.
const (
	OpHI    = "HI"    // C→S  optional hello on bus open: HI,<proto>
	OpREADY = "READY" // S→C  bus is up; client may begin the ping chain
	OpPING  = "PING"  // C→S  latency probe: PING,<id> (uint32, client-owned)
	OpPONG  = "PONG"  // S→C  echo: PONG,<id>;TIME,<nanos> (id verbatim)
	OpBYE   = "BYE"   // C→S  graceful bus close
	OpERR   = "ERR"   // S→C  non-fatal protocol error: ERR,<code>,<text>
)
