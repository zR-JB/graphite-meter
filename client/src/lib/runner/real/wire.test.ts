import { test, expect } from "bun:test";
import { encode, decode, DecodeError, type Frame } from "./wire";

// The SAME fixture the Go codec asserts against (go/internal/wire/frame_test.go).
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
    if (parts.length !== 3)
      throw new Error(`line ${i + 1}: want 3 fields: ${line}`);
    const dir = parts[0].trim();
    if (dir !== "encode" && dir !== "decode")
      throw new Error(`line ${i + 1}: bad dir ${dir}`);
    rows.push({
      line: i + 1,
      dir,
      input: parts[1].trim(),
      expected: parts[2].trim(),
    });
  }
  if (rows.length === 0) throw new Error("corpus is empty");
  return rows;
}

// Canonical "op=…;k=v;…" render the decode rows pin: the TS mirror of the corpus
// expected column (test artifact; the codec only emits on-wire frames).
function render(frame: Frame): string {
  switch (frame.op) {
    case "READY":
      return "op=READY";
    case "BYE":
      return "op=BYE";
    case "PING":
      return `op=PING;id=${frame.id}`;
    case "PONG":
      return `op=PONG;id=${frame.id};nanos=${frame.nanos}`;
    case "HI":
      return `op=HI;proto=${frame.proto}`;
    case "ERR":
      return `op=ERR;code=${frame.code};text=${frame.text}`;
  }
}

// Turn an "op=…;k=v;…" spec (encode rows' input column) into a Frame to feed encode.
function parseCanonical(spec: string): Frame {
  const fields = new Map<string, string>();
  for (const kv of spec.split(";")) {
    const eq = kv.indexOf("=");
    fields.set(kv.slice(0, eq), kv.slice(eq + 1));
  }
  const op = fields.get("op");
  switch (op) {
    case "READY":
      return { op: "READY" };
    case "BYE":
      return { op: "BYE" };
    case "PING":
      return { op: "PING", id: Number(fields.get("id")) };
    case "PONG":
      return {
        op: "PONG",
        id: Number(fields.get("id")),
        nanos: fields.get("nanos")!,
      };
    case "HI":
      return { op: "HI", proto: fields.get("proto")! };
    case "ERR":
      return {
        op: "ERR",
        code: fields.get("code")!,
        text: fields.get("text") ?? "",
      };
    default:
      throw new Error(`unknown canonical op ${op}`);
  }
}

const rows = parseCorpus(await Bun.file(corpusPath).text());

for (const row of rows) {
  test(`${row.dir} L${row.line}: ${row.input}`, () => {
    if (row.dir === "encode") {
      expect(encode(parseCanonical(row.input))).toBe(row.expected);
      return;
    }
    const wantErr = row.expected.startsWith("ERR:")
      ? row.expected.slice(4)
      : null;
    if (wantErr === null) {
      expect(render(decode(row.input))).toBe(row.expected);
      return;
    }
    let thrown: unknown;
    try {
      decode(row.input);
    } catch (error) {
      thrown = error;
    }
    expect(thrown).toBeInstanceOf(DecodeError);
    expect((thrown as DecodeError).code).toBe(wantErr);
  });
}
