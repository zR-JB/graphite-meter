/**
 * Client half of the encoding evidence behind the ping bus keeping a text codec
 * (the Go half is go/internal/wire/encoding_bench_test.go). Run with
 * `just bench-wire`; not a *.test.ts, so it stays out of the CI gate.
 *
 * The client's cost matters for a different reason than the server's: it lands
 * inside the RTT being measured, so what counts here is jitter, not throughput.
 *
 * Measured under bun (JavaScriptCore) on an 8-core box, ns/op:
 *
 *   decode text            ~170-186   decode JSON         ~109
 *   decode JSON + BigInt      ~141    decode binary        ~19
 *   encode text                ~12    encode JSON          ~47
 *
 * The ordering inverts the server's: JSON.parse is native and beats a
 * hand-rolled JS parser, where in Go the hand-rolled one wins 16x. It does not
 * change the decision, because the server aggregates every client's pings while
 * a client parses only its own: at a few hundred per second every row here is
 * under 0.05% of a core, so the server's 8x is what the choice turns on.
 *
 * Text decode was ~213ns while PONG.nanos was parsed into a BigInt. Holding it
 * as digits removed roughly a fifth of the cost, not the order of magnitude the
 * isolated field bench below suggests: the rest of decode is opcode dispatch,
 * six slices and two digit scans, none of which the field bench performs. Text
 * remains about 9x binary here, and that is still not worth a migration at a
 * few hundred pings per second.
 *
 * Browsers run V8 and SpiderMonkey rather than JavaScriptCore, so treat the
 * absolute numbers as indicative and the ordering as the finding.
 */
import { decode, encode } from "./wire";

const N = 2_000_000;

const textPong = encode({ op: "PONG", id: 4242424, nanos: "1234567890123" });
const jsonPong = JSON.stringify({
  op: "PONG",
  id: 4242424,
  nanos: "1234567890123",
});
const binPong = new DataView(new ArrayBuffer(13));
binPong.setUint8(0, 3);
binPong.setUint32(1, 4242424);
binPong.setBigUint64(5, 1234567890123n);

let sink: unknown;

function bench(name: string, fn: () => void): void {
  fn();
  const started = performance.now();
  for (let i = 0; i < N; i++) fn();
  const ns = ((performance.now() - started) * 1e6) / N;
  console.log(`${name.padEnd(32)} ${ns.toFixed(1).padStart(7)} ns/op`);
}

console.log(`wire codec, ${N.toLocaleString()} iterations each\n`);

bench("decode text (current)", () => {
  sink = decode(textPong);
});
bench("decode JSON", () => {
  sink = JSON.parse(jsonPong);
});
bench("decode JSON + BigInt", () => {
  const v = JSON.parse(jsonPong) as { op: string; id: number; nanos: string };
  sink = { op: v.op, id: v.id, nanos: BigInt(v.nanos) };
});
bench("decode binary (DataView)", () => {
  sink = {
    op: binPong.getUint8(0),
    id: binPong.getUint32(1),
    nanos: binPong.getBigUint64(5),
  };
});
bench("encode text (current)", () => {
  sink = encode({ op: "PING", id: 4242424 });
});
bench("encode JSON", () => {
  sink = JSON.stringify({ op: "PING", id: 4242424 });
});

// The client never reads PONG.nanos: the server clock is diagnostics-only and
// nothing in ping-worker or latencyChannel touches it. This is what parsing it
// anyway costs, isolated from the rest of decode.
console.log("\nthe unread nanos field:");
bench("PONG fields + BigInt nanos", () => {
  const comma = textPong.indexOf(",");
  const semi = textPong.indexOf(";", comma + 1);
  const timeComma = textPong.indexOf(",", semi + 1);
  sink = {
    op: "PONG",
    id: Number(textPong.slice(comma + 1, semi)),
    nanos: BigInt(textPong.slice(timeComma + 1)),
  };
});
bench("PONG fields, nanos untouched", () => {
  const comma = textPong.indexOf(",");
  const semi = textPong.indexOf(";", comma + 1);
  sink = { op: "PONG", id: Number(textPong.slice(comma + 1, semi)) };
});

console.log(
  `\nsizes: text=${textPong.length}B json=${jsonPong.length}B binary=${binPong.byteLength}B`,
);
if (sink === undefined) throw new Error("benchmark result unused");
