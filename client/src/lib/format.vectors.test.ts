import { expect, test } from "bun:test";
import { fixedMs, fmtBytes, fmtSpeed } from "./format";
import { formatHistoryRate } from "./history/format";

const vectors: Record<
  "ms" | "speed" | "bytes",
  { in: number; out: string }[]
> & { rate: { bytesPerSec: number; out: string }[] } = await Bun.file(
  new URL("../../../api/format.testvectors.json", import.meta.url),
).json();

test("formatting matches the shared vectors", () => {
  for (const { in: ms, out } of vectors.ms) expect(fixedMs(ms)).toBe(out);
  for (const { in: value, out } of vectors.speed)
    expect(fmtSpeed(value)).toBe(out);
  for (const { in: bytes, out } of vectors.bytes)
    expect(fmtBytes(bytes, "base10")).toBe(out);
  for (const { bytesPerSec, out } of vectors.rate)
    expect(
      formatHistoryRate(bytesPerSec, { base: "base10", kind: "bits" }),
    ).toBe(out);
});
