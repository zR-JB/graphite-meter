import { test, expect } from "bun:test";
import { interpolateAt, interpolateConnectedAt } from "./hoverInterp";

const pts = [
  { t: 0, v: 0 },
  { t: 10, v: 100 },
  { t: 20, v: 300 },
];
const pick = (s: { t: number; v: number }) => s.v;

test("interpolateAt: exact hit on the first sample", () => {
  expect(interpolateAt(pts, 0, pick)).toBe(0);
});

test("interpolateAt: exact hit on the last sample", () => {
  expect(interpolateAt(pts, 20, pick)).toBe(300);
});

test("interpolateAt: exact hit on an interior sample tick", () => {
  expect(interpolateAt(pts, 10, pick)).toBe(100);
});

test("interpolateAt: interpolated midpoint between two samples", () => {
  expect(interpolateAt(pts, 5, pick)).toBe(50); // midway 0..100
  expect(interpolateAt(pts, 15, pick)).toBe(200); // midway 100..300
});

test("interpolateAt: off-center weighting between two samples", () => {
  // t=12 is 20% of the way from t=10 (v=100) to t=20 (v=300).
  expect(interpolateAt(pts, 12, pick)).toBeCloseTo(140, 5);
});

test("interpolateAt: before the first sample returns null", () => {
  expect(interpolateAt(pts, -5, pick)).toBeNull();
});

test("interpolateAt: after the last sample returns null", () => {
  expect(interpolateAt(pts, 25, pick)).toBeNull();
});

test("interpolateAt: empty array returns null", () => {
  expect(interpolateAt([], 10, pick)).toBeNull();
});

test("interpolateAt: single-sample array only matches its own t", () => {
  const one = [{ t: 5, v: 42 }];
  expect(interpolateAt(one, 5, pick)).toBe(42);
  expect(interpolateAt(one, 6, pick)).toBeNull();
});

test("interpolateAt: large histories use logarithmic lookup", () => {
  const history = Array.from({ length: 100_000 }, (_, t) => ({
    t,
    v: t,
  }));
  let examined = 0;
  const observed = new Proxy(history, {
    get(target, property, receiver) {
      if (typeof property === "string" && /^\d+$/.test(property)) examined++;
      return Reflect.get(target, property, receiver);
    },
  });
  const value = interpolateAt(observed, 50_000.5, pick);

  expect(value).toBe(50_000.5);
  expect(examined).toBeLessThan(24);
});

test("interpolateConnectedAt refuses to invent a value across a break", () => {
  const samples = [
    { t: 0, v: 10, segment: 1 },
    { t: 10, v: 20, segment: 2 },
  ];
  expect(
    interpolateConnectedAt(
      samples,
      5,
      (sample) => sample.v,
      (left, right) => left.segment === right.segment,
    ),
  ).toBeNull();
});
