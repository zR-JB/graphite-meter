import { expect, test } from "bun:test";
import { interpolateConnectedAt } from "./hoverInterp";
import { monotoneCurve } from "./smoothPath";

interface Sample {
  t: number;
  value: number;
  run: number;
}
const sample = (t: number, run: number, value: number): Sample => ({
  t,
  value,
  run,
});
const connected = (left: Sample, right: Sample) => left.run === right.run;

test("hover follows the rendered curve, preserving knots and continuity boundaries", () => {
  const samples = [
    sample(0, 4, 0),
    sample(100, 4, 800),
    sample(350, 4, 1_000),
    sample(600, 4, 400),
    sample(900, 5, 9_000),
  ];
  const at = (t: number) =>
    interpolateConnectedAt(samples, t, (s) => s.value, connected);
  const curve = monotoneCurve(
    samples.slice(0, 4).map((s) => ({ x: s.t, y: s.value })),
  );
  for (let i = 0; i < 3; i++) {
    for (const u of [0.25, 0.5, 0.75]) {
      const v = 1 - u;
      const segment = curve[i];
      const expected =
        v ** 3 * samples[i].value +
        3 * v ** 2 * u * segment.control1.y +
        3 * v * u ** 2 * segment.control2.y +
        u ** 3 * segment.end.y;
      expect(
        at(samples[i].t + u * (samples[i + 1].t - samples[i].t)),
      ).toBeCloseTo(expected, 8);
    }
  }
  for (const s of samples) expect(at(s.t)).toBe(s.value);
  expect(at(225)).not.toBe(900); // A straight chord would miss the drawn curve.
  expect(at(750)).toBeNull(); // An explicit continuity break is a hover break.
  expect(at(-1)).toBeNull();
  expect(at(901)).toBeNull();
});
