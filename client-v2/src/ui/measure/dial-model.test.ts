import { expect, test } from "bun:test";
import type { ThroughputResult } from "../../engine/measure/aggregate";
import { fmtTick } from "../../view/format";
import {
  beadKinds,
  DIAL_LABELS,
  dialLayout,
  DRAIN_MS,
  gaugeFraction,
  gaugeValue,
  headlineArc,
  Needle,
  reachMs,
  resultArcs,
  resultSweepMs,
  RISE_MS,
} from "./dial-model";

test("the ring fills the measured face, keeps its labels inside and a hung note under its tick ends", () => {
  // face width, height, hung note → radius, arc width, centre, note top and width, the face's type measure
  const cases = [
    [300, 320, 43.1, 107.6, 14.0, 150, 179.7, 276.9, 173.8, 258.3],
    [417.6, 400, 43.1, 148.5, 19.3, 208.8, 226.5, 356.9, 240.1, 356.5],
    [560, 482, 43.1, 190.5, 24.8, 280, 274.6, 438.9, 308.0, 457.2],
    [300, 336.9, 0, 109.2, 14.2, 150, 188.4, 286.9, 176.3, 262.0],
    [358, 257.8, 0, 100.8, 13.1, 179, 147.4, 239.1, 162.8, 241.9],
    [308.6, 289.8, 0, 112.9, 14.7, 154.3, 165.4, 267.0, 182.4, 271.0],
  ] as const;
  for (const [width, height, note, ...expected] of cases) {
    const layout = dialLayout(width, height, note);
    const got = [
      layout.radius,
      layout.arcWidth,
      layout.center.x,
      layout.center.y,
      layout.noteTop,
      layout.noteWidth,
      layout.faceMin,
    ];
    got.forEach((value, i) => expect(value).toBeCloseTo(expected[i]!, 1));
    for (const { x, y } of layout.labels) {
      expect(x).toBeGreaterThanOrEqual(0);
      expect(x).toBeLessThanOrEqual(width);
      expect(y).toBeGreaterThanOrEqual(0);
      expect(y).toBeLessThanOrEqual(height);
    }
  }
  expect(dialLayout(0, 0).radius).toBe(36);
  expect(dialLayout(300, 320).labels.map(({ anchorX, anchorY }) => `${anchorX} ${anchorY}`)).toEqual([
    "end start",
    "end end",
    "center end",
    "start end",
    "start start",
  ]);
});

test("the transfer curve is monotonic, bounded and invertible, and a gigabit ring reads round labels", () => {
  const scale = 1_000;
  const knots = [0, 5, 10, 50, 100, 250, 500, 750, 1_000];
  const values = [...knots, ...knots.slice(1).map((v, i) => (v + knots[i]!) / 2)].sort((a, b) => a - b);
  let previous = -1;
  for (const value of values) {
    const fraction = gaugeFraction(value, scale);
    expect(fraction).toBeGreaterThanOrEqual(Math.max(0, previous));
    expect(fraction).toBeLessThanOrEqual(1);
    expect(gaugeValue(fraction, scale)).toBeCloseTo(value, 8);
    previous = fraction;
  }
  for (const odd of [Number.NaN, -5, Number.POSITIVE_INFINITY]) expect(gaugeFraction(odd, scale)).toBe(odd > 0 ? 1 : 0);
  expect(gaugeFraction(5, 0)).toBe(1);
  const gigabit = 125e6;
  const mbit = (bytesPerSec: number) => (bytesPerSec * 8) / 1e6;
  expect(DIAL_LABELS.map((fraction) => fmtTick(mbit(gaugeValue(fraction, gigabit))))).toEqual([
    "0",
    "10",
    "100",
    "500",
    "1000",
  ]);
  expect(gaugeFraction(12.5e6, gigabit)).toBeCloseTo(0.5, 10);
  expect(gaugeFraction(117.5e6, gigabit)).toBeCloseTo(0.97, 2);
});

const result = (bytesPerSec: number): ThroughputResult => ({
  peakBytesPerSec: bytesPerSec,
  stabilityPct: 100,
  totalBytes: 0,
  reportedBytesPerSec: bytesPerSec,
});
const bidirectional = (down: number | null, up: number | null) => ({
  down: down === null ? null : result(down),
  up: up === null ? null : result(up),
  wire: null,
});

test("result arcs lie longest first, need both lanes for a combined value, and read out by role", () => {
  const arcs = (download: number | null, upload: number | null, bidi: ReturnType<typeof bidirectional> | null) =>
    resultArcs({
      download: download === null ? null : { ...result(download), wire: null },
      upload: upload === null ? null : { ...result(upload), wire: null },
      bidirectional: bidi,
    }).map(
      ({ stage, direction, bytesPerSec, dashed }) => `${stage}:${direction}:${bytesPerSec}${dashed ? ":dashed" : ""}`,
    );
  expect(arcs(10, 20, bidirectional(30, 40))).toEqual([
    "bidirectional:bidirectional:70",
    "upload:upload:20",
    "download:download:10",
  ]);
  expect(arcs(null, null, bidirectional(30, null))).toEqual(["bidirectional:download:30:dashed"]);
  expect(arcs(null, null, bidirectional(null, 0))).toEqual(["bidirectional:upload:0:dashed"]);
  expect(arcs(null, null, bidirectional(0, 40))).toEqual(["bidirectional:bidirectional:40"]);
  expect(arcs(null, null, bidirectional(null, null))).toEqual([]);
  expect(resultArcs(null)).toEqual([]);
  const all = resultArcs({
    download: { ...result(5), wire: null },
    upload: { ...result(50), wire: null },
    bidirectional: bidirectional(60, 60),
  });
  expect(headlineArc(all)?.stage).toBe("download");
  expect(headlineArc(all.filter((arc) => arc.stage !== "download"))?.stage).toBe("upload");
  expect(headlineArc([])).toBeNull();
});

test("heads stay whole beads apart, and lie over a close higher one cut out or halved", () => {
  // A 72 px ring with 8 px heads: 18 px apart along the ring is the stacking distance, 8 px the split one.
  expect(beadKinds([1, 0.5, 0], 72, 8)).toEqual(["bead", "bead", "bead"]);
  expect(beadKinds([0.6, 0.5, 0.46], 72, 8)).toEqual(["bead", "bead", "stacked"]);
  expect(beadKinds([0.5, 0.5, 0.49], 72, 8)).toEqual(["bead", "split", "split"]);
  expect(beadKinds([Number.NaN, 0], 72, 8)).toEqual(["bead", "split"]);
});

test("a result sweeps the rest of the ring and each bead pops as the front reaches it", () => {
  expect([resultSweepMs(0), resultSweepMs(0.5), resultSweepMs(0.9)]).toEqual([900, 450, 360]);
  expect(reachMs(0.3, 0.4, 540)).toBe(0);
  expect(reachMs(1, 0, 900)).toBe(900);
  expect(reachMs(1, 1, 360)).toBe(0);
  // Ease-out cubic: the front covers seven eighths of the way in half the time.
  expect(reachMs(0.875, 0, 900)).toBeCloseTo(450, 6);
});

test("the needle rises, follows, glides across a rescale, drains, and leaves the ring to a result", () => {
  const needle = new Needle();
  const shown = (course = "a", motion = true) => ({ visible: true, hold: false, motion, course });
  needle.update(shown(), 0);
  expect(needle.angle(0, 200)).toBe(0);
  expect(needle.angle(RISE_MS / 2, 200)).toBeCloseTo(100, 6);
  expect(needle.angle(RISE_MS, 180)).toBe(180);
  // Within the course it follows the live value exactly.
  needle.update(shown(), 600);
  expect(needle.angle(700, 120)).toBe(120);
  needle.update(shown("b"), 700);
  expect(needle.moving(800)).toBe(true);
  expect(needle.angle(700 + 360, 90)).toBe(90);
  // The value ends: it drains to zero, lit until it gets there.
  needle.update({ visible: false, hold: false, motion: true, course: "b" }, 2_000);
  expect(needle.lit(false, true)).toBe(true);
  expect(needle.angle(2_000 + DRAIN_MS / 2, null)).toBeCloseTo(45, 6);
  expect(needle.angle(2_000 + DRAIN_MS, null)).toBe(0);
  expect(needle.lit(false, true)).toBe(false);
  // A speed result holds the needle, then takes over from where it stands.
  needle.update(shown("b"), 3_000);
  needle.angle(3_000 + RISE_MS, 135);
  needle.update({ visible: false, hold: true, motion: true, course: "b" }, 4_000);
  expect(needle.angle(4_100, null)).toBe(135);
  expect(needle.takeover(true, 4_100)).toBe(0.5);
  expect(needle.lit(false, true)).toBe(false);
  expect(needle.angle(5_000, null)).toBe(135);
  // The next stage rises from zero.
  needle.update(shown("c"), 6_000);
  expect(needle.angle(6_000, 50)).toBe(0);
  // A rescale that interrupts a rise carries the needle's speed on rather than setting off from rest.
  const rising = new Needle();
  rising.update(shown("a"), 0);
  for (let now = 0; now <= 256; now += 16) rising.angle(now, 200);
  const before = rising.angle(272, 200);
  rising.update(shown("b"), 272);
  expect(rising.angle(288, 100) - before).toBeGreaterThan(5);
  // Unseen or reduced, every change snaps and a result sweeps from zero.
  const still = new Needle();
  still.update(shown("a", false), 0);
  expect(still.angle(0, 77)).toBe(77);
  expect(still.takeover(false, 10)).toBe(0);
});
