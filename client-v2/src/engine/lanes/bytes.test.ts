import { expect, test } from "bun:test";
import { largestPost } from "./bytes";

const MiB = 1024 * 1024;

test("a lane's largest POST is v1's device-memory share divided across the lanes", () => {
  for (const [lanes, deviceMemory, expected] of [
    [1, 8, 256 * MiB],
    [4, 8, 64 * MiB],
    [4, 2, 4 * MiB],
    [4, 4, 6 * MiB],
    [16, 2, 2 * MiB],
    [0, 8, 256 * MiB],
    [1, undefined, 128 * MiB],
    [4, undefined, 32 * MiB],
    [128, undefined, 2 * MiB],
  ] as const)
    expect(largestPost(lanes, deviceMemory), `${lanes} lanes, ${deviceMemory} GB`).toBe(expected);
});
