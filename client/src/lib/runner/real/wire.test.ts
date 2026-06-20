import { test, expect } from "bun:test";
import { encode, decode, DecodeError, type Frame } from "./wire";

// The SAME fixture the Go codec asserts against (server/internal/wire/frame_test.go).
// Resolve it from this file's dir up to the repo root (real → runner → lib → src →
// client → repo). Bun exposes import.meta.dir.
const corpusPath = `${import.meta.dir}/../../../../../api/wire.testvectors.txt`;

interface Row {
  line: number;
  dir: "encode" | "decode";
  input: string;
  expected: string;
}

function parseCorpus(text: string): Row[] {
  const rows: Row[] = [];
  const lines = text.split("\n");
  for (let i = 0; i < lines.length; i++) {
    const line = lines[i].trim();
    if (line === "" || line.startsWith("#")) continue;
    const parts = line.split("|");
    if (parts.length !== 3) throw new Error(`line ${i + 1}: want 3 fields: ${line}`);
    const dir = parts[0].trim();
    if (dir !== "encode" && dir !== "decode") throw new Error(`line ${i + 1}: bad dir ${dir}`);
    rows.push({ line: i + 1, dir, input: parts[1].trim(), expected: parts[2].trim() });
  }
  if (rows.length === 0) throw new Error("corpus is empty");
  return rows;
}

// Canonical "op=…;k=v;…" render the decode rows pin — the TS mirror of the corpus
// expected column (test artifact; the codec only emits on-wire frames).
function render(f: Frame): string {
  switch (f.op) {
    case "READY":
      return "op=READY";
    case "BYE":
      return "op=BYE";
    case "PING":
      return `op=PING;id=${f.id}`;
    case "PONG":
      return `op=PONG;id=${f.id};nanos=${f.nanos}`;
    case "SIZE":
      return `op=SIZE;bytes=${f.bytes}`;
    case "HI":
      return `op=HI;proto=${f.proto}`;
    case "BYTES_RECEIVED":
      return `op=BYTES_RECEIVED;n=${f.n}`;
    case "UPLOAD_COMPLETE":
      return `op=UPLOAD_COMPLETE;n=${f.n}`;
    case "ERR":
      return `op=ERR;code=${f.code};text=${f.text}`;
  }
}

// Turn an "op=…;k=v;…" spec (encode rows' input column) into a Frame to feed encode.
function parseCanonical(spec: string): Frame {
  const m = new Map<string, string>();
  for (const kv of spec.split(";")) {
    const e = kv.indexOf("=");
    m.set(kv.slice(0, e), kv.slice(e + 1));
  }
  const op = m.get("op");
  switch (op) {
    case "READY":
      return { op: "READY" };
    case "BYE":
      return { op: "BYE" };
    case "PING":
      return { op: "PING", id: Number(m.get("id")) };
    case "PONG":
      return { op: "PONG", id: Number(m.get("id")), nanos: BigInt(m.get("nanos")!) };
    case "SIZE":
      return { op: "SIZE", bytes: BigInt(m.get("bytes")!) };
    case "HI":
      return { op: "HI", proto: m.get("proto")! };
    case "BYTES_RECEIVED":
      return { op: "BYTES_RECEIVED", n: BigInt(m.get("n")!) };
    case "UPLOAD_COMPLETE":
      return { op: "UPLOAD_COMPLETE", n: BigInt(m.get("n")!) };
    case "ERR":
      return { op: "ERR", code: m.get("code")!, text: m.get("text") ?? "" };
    default:
      throw new Error(`unknown canonical op ${op}`);
  }
}

const rows = parseCorpus(await Bun.file(corpusPath).text());

for (const r of rows) {
  test(`${r.dir} L${r.line}: ${r.input}`, () => {
    if (r.dir === "decode") {
      const wantErr = r.expected.startsWith("ERR:") ? r.expected.slice(4) : null;
      if (wantErr !== null) {
        let thrown: unknown;
        try {
          decode(r.input);
        } catch (e) {
          thrown = e;
        }
        expect(thrown).toBeInstanceOf(DecodeError);
        expect((thrown as DecodeError).code).toBe(wantErr);
      } else {
        expect(render(decode(r.input))).toBe(r.expected);
      }
    } else {
      expect(encode(parseCanonical(r.input))).toBe(r.expected);
    }
  });
}
