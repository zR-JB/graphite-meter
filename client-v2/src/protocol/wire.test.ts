import { expect, test } from "bun:test";
import { rows } from "./pins.testutil";
import { ping, pong } from "./wire";

// Go checks all four directions of the corpus; the browser encodes pings and decodes pongs.
test("the shared wire corpus", async () => {
  for (const [operation, input, expected] of await rows("wire.testvectors.txt")) {
    if (operation === "encode-ping") expect(ping(Number(input))).toBe(expected!);
    if (operation !== "decode-pong") continue;
    const reply = pong(input!);
    if (expected === "INVALID") expect(reply).toBeNull();
    else {
      const [id, ns] = expected!.split(",");
      const exact = BigInt(ns!) <= BigInt(Number.MAX_SAFE_INTEGER);
      expect(reply).toEqual({ id: Number(id), handlingNs: exact ? Number(ns) : null });
    }
  }
});
