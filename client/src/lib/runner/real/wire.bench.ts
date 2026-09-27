// Measure the shipped codec; fixture alternatives do not exercise production code.
import { decodePong, encodePing } from "./wire";

const N = 2_000_000;
const pong = "PONG,4242424,123456789";
let sink: unknown;

function bench(name: string, fn: () => void): void {
  fn();
  const started = Bun.nanoseconds();
  for (let i = 0; i < N; i++) fn();
  const ns = (Bun.nanoseconds() - started) / N;
  console.log(`${name.padEnd(20)} ${ns.toFixed(1).padStart(7)} ns/op`);
}

console.log(`wire codec, ${N.toLocaleString()} iterations each`);
bench("decode PONG", () => {
  sink = decodePong(pong);
});
console.log("decoded:", JSON.stringify(sink));
bench("encode PING", () => {
  sink = encodePing(4242424);
});
console.log("encoded:", sink);
