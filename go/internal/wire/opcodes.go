package wire

// Opcode keyword table — the Go half of the cross-language pin. The TS half is
// client/src/lib/runner/real/wire.ts. These literal strings are normative and
// MUST match every language's implementation. Frames are message-delimited ASCII —
// one logical message per WS frame / WT datagram, parsed by slicing on ','.
const (
	OpHI             = "HI"              // C→S  optional hello on bus open: HI,<proto>
	OpREADY          = "READY"           // S→C  bus is up; client may begin the ping chain
	OpPING           = "PING"            // C→S  latency probe: PING,<id> (uint32, client-owned)
	OpPONG           = "PONG"            // S→C  echo: PONG,<id>;TIME,<nanos> (id verbatim)
	OpSIZE           = "SIZE"            // C→S  wt download request: SIZE,<bytes>
	OpBytesReceived  = "BYTES_RECEIVED"  // S→C  running server-measured upload count + active time: BYTES_RECEIVED,<n>;TIME,<activeNanos>
	OpUploadComplete = "UPLOAD_COMPLETE" // S→C  final server-measured upload total + active time: UPLOAD_COMPLETE,<n>;TIME,<activeNanos>
	OpBYE            = "BYE"             // C→S  graceful bus close
	OpERR            = "ERR"             // S→C  non-fatal protocol error: ERR,<code>,<text>
)
