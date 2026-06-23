/**
 * Message-bus wire codec (api/wire.md) — the TS half of the cross-language pin;
 * the Go half is go/internal/wire/{opcodes,frame}.go. Both encoders/decoders
 * assert byte-for-byte against the shared corpus api/wire.testvectors.txt
 * (wire.test.ts here, frame_test.go there).
 *
 * Framing is message-delimited ASCII — one logical message per WS frame / WT
 * datagram, parsed by indexOf(',') slicing, never JSON, never regex. This module
 * is imported by the ping worker (workers/ping-worker.ts); the main thread never
 * touches frames.
 */

/** Opcode keyword table — the literal uppercase keywords from api/wire.md
 *  §Opcodes, pinned here as the TS mirror of go/internal/wire/opcodes.go. */
export const Op = {
  HI: "HI",
  READY: "READY",
  PING: "PING",
  PONG: "PONG",
  SIZE: "SIZE",
  BYTES_RECEIVED: "BYTES_RECEIVED",
  UPLOAD_COMPLETE: "UPLOAD_COMPLETE",
  BYE: "BYE",
  ERR: "ERR",
} as const;

/**
 * A parsed wire frame. `id` is a uint32 (safe as a JS number); `nanos`, `bytes`,
 * and `n` are uint64 and MUST be `bigint` so the boundary value
 * 18446744073709551615 round-trips byte-exact (it exceeds Number.MAX_SAFE_INTEGER).
 */
export type Frame =
  | { op: "READY" }
  | { op: "BYE" }
  | { op: "PING"; id: number }
  | { op: "PONG"; id: number; nanos: bigint }
  | { op: "SIZE"; bytes: bigint }
  | { op: "HI"; proto: string }
  | { op: "BYTES_RECEIVED"; n: bigint; nanos: bigint }
  | { op: "UPLOAD_COMPLETE"; n: bigint; nanos: bigint }
  | { op: "ERR"; code: string; text: string };

/** Stable rejection codes a receiver echoes as ERR,<code>,<text>. */
export const ErrBadOp = "bad_op"; // unknown opcode keyword
export const ErrBadArgs = "bad_args"; // opcode known, args missing/malformed

/** Thrown by decode for a malformed frame; `code` is the token to echo as ERR. */
export class DecodeError extends Error {
  code: string;
  text: string;
  constructor(code: string, text: string) {
    super(`${code}: ${text}`);
    this.name = "DecodeError";
    this.code = code;
    this.text = text;
  }
}

const MAX_U32 = 4294967295n;
const MAX_U64 = 18446744073709551615n;

/** Parse a bare unsigned decimal integer the same way Go's strconv.ParseUint
 *  does: digits only, non-empty, no sign/whitespace. Returns null on reject. No
 *  regex — a plain char-code scan keeps the framing parse allocation-free. */
function parseUint(s: string): bigint | null {
  if (s.length === 0) return null;
  for (let i = 0; i < s.length; i++) {
    const ch = s.charCodeAt(i);
    if (ch < 48 || ch > 57) return null; // not 0-9
  }
  return BigInt(s);
}

function u32(s: string, what: string): number {
  const v = parseUint(s);
  if (v === null || v > MAX_U32) throw new DecodeError(ErrBadArgs, what);
  return Number(v);
}

function u64(s: string, what: string): bigint {
  const v = parseUint(s);
  if (v === null || v > MAX_U64) throw new DecodeError(ErrBadArgs, what);
  return v;
}

/** Parse the "<n>;TIME,<nanos>" body shared by BYTES_RECEIVED and UPLOAD_COMPLETE:
 *  a cumulative server byte total plus the server's ACTIVE measurement time (ns the
 *  server was actually draining bytes, dead zones excluded) it was sampled at. The
 *  client divides Δn by Δnanos over this server clock. Reuses the PONG `;TIME`
 *  framing (and Go's parseCountTime) — but PONG's nanos is a raw clock, while here
 *  it is active-transfer time. */
function countTime(rest: string, op: string): [bigint, bigint] {
  const s = rest.indexOf(";");
  if (s === -1) throw new DecodeError(ErrBadArgs, `${op} TIME`);
  const n = u64(rest.slice(0, s), `${op} n`);
  const tail = rest.slice(s + 1);
  const tc = tail.indexOf(",");
  if (tc === -1 || tail.slice(0, tc) !== "TIME") throw new DecodeError(ErrBadArgs, `${op} TIME`);
  return [n, u64(tail.slice(tc + 1), `${op} nanos`)];
}

/** Parse one on-wire message into a Frame. Throws DecodeError(bad_op) on an
 *  unknown opcode and DecodeError(bad_args) on missing/malformed args. */
export function decode(msg: string): Frame {
  const c = msg.indexOf(",");
  const op = c === -1 ? msg : msg.slice(0, c);
  const rest = c === -1 ? "" : msg.slice(c + 1);

  switch (op) {
    case Op.READY:
      return { op: "READY" };
    case Op.BYE:
      return { op: "BYE" };

    case Op.PING:
      return { op: "PING", id: u32(rest, "PING id") };

    case Op.PONG: {
      // rest = "<id>;TIME,<nanos>"
      const s = rest.indexOf(";");
      if (s === -1) throw new DecodeError(ErrBadArgs, "PONG TIME");
      const id = u32(rest.slice(0, s), "PONG id");
      const tail = rest.slice(s + 1);
      const tc = tail.indexOf(",");
      if (tc === -1 || tail.slice(0, tc) !== "TIME") throw new DecodeError(ErrBadArgs, "PONG TIME");
      const nanos = u64(tail.slice(tc + 1), "PONG nanos");
      return { op: "PONG", id, nanos };
    }

    case Op.SIZE:
      return { op: "SIZE", bytes: u64(rest, "SIZE bytes") };

    case Op.HI:
      if (rest === "") throw new DecodeError(ErrBadArgs, "HI proto");
      return { op: "HI", proto: rest };

    case Op.BYTES_RECEIVED: {
      const [n, nanos] = countTime(rest, "BYTES_RECEIVED");
      return { op: "BYTES_RECEIVED", n, nanos };
    }

    case Op.UPLOAD_COMPLETE: {
      const [n, nanos] = countTime(rest, "UPLOAD_COMPLETE");
      return { op: "UPLOAD_COMPLETE", n, nanos };
    }

    case Op.ERR: {
      const ec = rest.indexOf(",");
      const code = ec === -1 ? rest : rest.slice(0, ec);
      const text = ec === -1 ? "" : rest.slice(ec + 1);
      if (code === "") throw new DecodeError(ErrBadArgs, "ERR code");
      return { op: "ERR", code, text };
    }

    default:
      throw new DecodeError(ErrBadOp, op);
  }
}

/** Render a Frame to its exact on-wire string. */
export function encode(f: Frame): string {
  switch (f.op) {
    case "READY":
      return Op.READY;
    case "BYE":
      return Op.BYE;
    case "PING":
      return `${Op.PING},${f.id}`;
    case "PONG":
      return `${Op.PONG},${f.id};TIME,${f.nanos}`;
    case "SIZE":
      return `${Op.SIZE},${f.bytes}`;
    case "HI":
      return `${Op.HI},${f.proto}`;
    case "BYTES_RECEIVED":
      return `${Op.BYTES_RECEIVED},${f.n};TIME,${f.nanos}`;
    case "UPLOAD_COMPLETE":
      return `${Op.UPLOAD_COMPLETE},${f.n};TIME,${f.nanos}`;
    case "ERR":
      return `${Op.ERR},${f.code},${f.text}`;
  }
}
