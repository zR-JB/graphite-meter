/* Only the ping worker imports this module. */

/** Opcode keyword table: uppercase keywords, the TS mirror of go/internal/wire/opcodes.go. */
const Op = {
  HI: "HI",
  READY: "READY",
  PING: "PING",
  PONG: "PONG",
  BYE: "BYE",
  ERR: "ERR",
} as const;

/* A parsed wire frame. */
export type Frame =
  | { op: "READY" }
  | { op: "BYE" }
  | { op: "PING"; id: number }
  | { op: "PONG"; id: number; nanos: string }
  | { op: "HI"; proto: string }
  | { op: "ERR"; code: string; text: string };

/** Stable rejection codes a receiver echoes as ERR,<code>,<text>. */
const ErrBadOp = "bad_op"; // unknown opcode keyword
const ErrBadArgs = "bad_args"; // opcode known, args missing/malformed

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

const MAX_U32 = 4294967295;
/* uint64 max as digits. */
const MAX_U64_DIGITS = "18446744073709551615";

/* Digits only, non-empty, no sign or whitespace: what Go's strconv.ParseUint accepts. */
function digitsOnly(s: string): boolean {
  if (s.length === 0) return false;
  for (let i = 0; i < s.length; i++) {
    const ch = s.charCodeAt(i);
    if (ch < 48 || ch > 57) return false;
  }
  return true;
}

/* A uint32 is under Number.MAX_SAFE_INTEGER, so it parses exactly as a Number. */
function u32(s: string, field: string): number {
  if (!digitsOnly(s) || s.length > 10) throw new DecodeError(ErrBadArgs, field);
  const v = Number(s);
  if (v > MAX_U32) throw new DecodeError(ErrBadArgs, field);
  return v;
}

/* A uint64 is kept as its digits: it exceeds Number.MAX_SAFE_INTEGER, and every consumer either re-emits it. */
function u64Digits(s: string, field: string): string {
  if (
    !digitsOnly(s) ||
    s.length > MAX_U64_DIGITS.length ||
    (s.length === MAX_U64_DIGITS.length && s > MAX_U64_DIGITS)
  )
    throw new DecodeError(ErrBadArgs, field);
  return s;
}

/* Parse one on-wire message into a Frame. */
export function decode(msg: string): Frame {
  const comma = msg.indexOf(",");
  const op = comma === -1 ? msg : msg.slice(0, comma);
  const rest = comma === -1 ? "" : msg.slice(comma + 1);

  switch (op) {
    case Op.READY:
      return { op: "READY" };
    case Op.BYE:
      return { op: "BYE" };

    case Op.PING:
      return { op: "PING", id: u32(rest, "PING id") };

    case Op.PONG: {
      // rest = "<id>;TIME,<nanos>"
      const semi = rest.indexOf(";");
      if (semi === -1) throw new DecodeError(ErrBadArgs, "PONG TIME");
      const id = u32(rest.slice(0, semi), "PONG id");
      const tail = rest.slice(semi + 1);
      const timeComma = tail.indexOf(",");
      if (timeComma === -1 || tail.slice(0, timeComma) !== "TIME")
        throw new DecodeError(ErrBadArgs, "PONG TIME");
      const nanos = u64Digits(tail.slice(timeComma + 1), "PONG nanos");
      return { op: "PONG", id, nanos };
    }

    case Op.HI:
      if (rest === "") throw new DecodeError(ErrBadArgs, "HI proto");
      return { op: "HI", proto: rest };

    case Op.ERR: {
      const codeComma = rest.indexOf(",");
      const code = codeComma === -1 ? rest : rest.slice(0, codeComma);
      const text = codeComma === -1 ? "" : rest.slice(codeComma + 1);
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
    case "HI":
      return `${Op.HI},${f.proto}`;
    case "ERR":
      return `${Op.ERR},${f.code},${f.text}`;
  }
}
