import { test, expect } from "bun:test";
import { laneUrl, PER_STREAM_BYTES } from "../paths";

const spec = {
  dir: "down" as const,
  base: "http://meter.test:7246",
  cbSeed: "r42",
};

test("fetch lanes carry a per-lane cache buster, and an upload its minted id once it exists", () => {
  expect(laneUrl(spec, 3)).toBe(
    `http://meter.test:7246/download?bytes=${PER_STREAM_BYTES}&cb=r42-3`,
  );
  const up = { ...spec, dir: "up" as const };
  expect(laneUrl(up, 1)).toBe("http://meter.test:7246/upload?cb=r42-1");
  expect(laneUrl(up, 1, "gmu_a/b")).toBe(
    "http://meter.test:7246/upload?cb=r42-1&id=gmu_a%2Fb",
  );
});
