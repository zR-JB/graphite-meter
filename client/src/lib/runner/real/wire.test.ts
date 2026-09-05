import { test, expect } from "bun:test";
import { encodePing, decodePong } from "./wire";

// The Go codec verifies all four directions; the client verifies its two directions.
const corpus = await Bun.file(
  `${import.meta.dir}/../../../../../api/wire.testvectors.txt`,
).text();
for (const line of corpus.split("\n")) {
  if (line.startsWith("#") || !line) continue;
  const [operation, input, expected] = line
    .split("|")
    .map((part) => part.trim());
  if (operation !== "encode-ping" && operation !== "decode-pong") continue;
  test(`${operation}: ${input}`, () => {
    if (operation === "encode-ping") {
      expect(encodePing(Number(input))).toBe(expected);
      return;
    }
    const pong = decodePong(input);
    if (expected === "INVALID") expect(pong).toBeNull();
    else {
      expect(pong).not.toBeNull();
      expect(`${pong!.id},${BigInt(pong!.handlingNanos)}`).toBe(expected);
    }
  });
}
