/* The client's cost matters for a different reason than the server's: it lands inside the RTT being measured, so. */
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

// The client never reads PONG.nanos: the server clock is diagnostics-only and nothing in ping-worker or.
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
