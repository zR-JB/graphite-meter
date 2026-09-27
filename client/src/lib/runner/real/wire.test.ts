import { test, expect } from "bun:test";
import { encodePing, decodePong } from "./wire";
import { readPin } from "../../test-helpers.testutil";

// The Go codec verifies all four directions; the client verifies its two directions.
for (const [operation, input, expected] of await readPin(
  "wire.testvectors.txt",
)) {
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
