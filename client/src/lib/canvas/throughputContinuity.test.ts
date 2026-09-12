import { expect, test } from "bun:test";
import type { ThroughputSample } from "../runner/contract";
import { interpolateConnectedAt } from "./hoverInterp";
import { monotoneCurve } from "./smoothPath";
import { throughputSamplesContinuous } from "./throughputContinuity";

const sample = (
  t: number,
  continuityId: number,
  bytesPerSec = 1_000,
): ThroughputSample => ({
  t,
  bytesPerSec,
  bytesCumulative: t,
  dir: "up",
  phase: "upload",
  continuityId,
});

test("an irregular authoritative throughput gap remains continuous", () => {
  const samples = [sample(100, 4, 1_000), sample(800, 4, 2_000)];

  expect(throughputSamplesContinuous(samples[0], samples[1])).toBe(true);
  expect(
    interpolateConnectedAt(
      samples,
      450,
      (entry) => entry.bytesPerSec,
      throughputSamplesContinuous,
    ),
  ).toBe(1_500);
});

test("an explicit continuity break remains a throughput and hover break", () => {
  const samples = [sample(100, 4, 1_000), sample(800, 5, 2_000)];

  expect(throughputSamplesContinuous(samples[0], samples[1])).toBe(false);
  expect(
    interpolateConnectedAt(
      samples,
      450,
      (entry) => entry.bytesPerSec,
      throughputSamplesContinuous,
    ),
  ).toBeNull();
});

test("hover follows the rendered curve, preserving knots and continuity boundaries", () => {
  const samples = [
    sample(0, 4, 0),
    sample(100, 4, 800),
    sample(350, 4, 1_000),
    sample(600, 4, 400),
    sample(900, 5, 9_000),
  ];
  const at = (t: number) =>
    interpolateConnectedAt(
      samples,
      t,
      (s) => s.bytesPerSec,
      throughputSamplesContinuous,
    );
  const curve = monotoneCurve(
    samples.slice(0, 4).map((s) => ({ x: s.t, y: s.bytesPerSec })),
  );
  for (let i = 0; i < 3; i++) {
    for (const u of [0.25, 0.5, 0.75]) {
      const v = 1 - u;
      const segment = curve[i];
      const expected =
        v ** 3 * samples[i].bytesPerSec +
        3 * v ** 2 * u * segment.control1.y +
        3 * v * u ** 2 * segment.control2.y +
        u ** 3 * segment.end.y;
      expect(
        at(samples[i].t + u * (samples[i + 1].t - samples[i].t)),
      ).toBeCloseTo(expected, 8);
    }
  }
  for (const s of samples) expect(at(s.t)).toBe(s.bytesPerSec);
  expect(at(225)).not.toBe(900); // A straight chord would miss the drawn curve.
  expect(at(750)).toBeNull();
  expect(at(-1)).toBeNull();
  expect(at(901)).toBeNull();
});
