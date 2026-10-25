import { expect, test } from "bun:test";
import type { ThroughputResult } from "../../engine/measure/aggregate";
import type { Cause, CellStatus, Grid } from "../../engine/run/result";
import { STAGES, type Stage } from "../../engine/run/schedule";
import { fmtTick } from "../../view/format";
import {
  beadKinds,
  DIAL_LABELS,
  dialLayout,
  DRAIN_MS,
  endingHint,
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
  for (const [width, height, note] of [
    [300, 320, 43.1],
    [417.6, 400, 43.1],
    [560, 482, 43.1],
    [300, 336.9, 0],
    [358, 257.8, 0],
    [308.6, 289.8, 0],
  ] as const) {
    const { center, radius, arcWidth, labels, noteTop, noteWidth, faceMin } = dialLayout(
      width,
      height,
      note,
    );
    expect(center.x).toBeCloseTo(width / 2, 6);
    expect(radius + arcWidth / 2).toBeLessThan(Math.min(width / 2, center.y));
    for (const { x, y } of labels) {
      expect(x).toBeGreaterThanOrEqual(0);
      expect(x).toBeLessThanOrEqual(width);
      expect(y).toBeGreaterThanOrEqual(0);
      expect(y).toBeLessThanOrEqual(height);
    }
    // A hung note starts under the lowest tick ends, fits between the lowest labels and inside the face.
    expect(noteTop).toBeGreaterThan(center.y + Math.SQRT1_2 * (radius + arcWidth / 2));
    expect(noteWidth).toBeLessThan(labels.at(-1)!.x - labels[0]!.x);
    if (note) expect(noteTop + note).toBeLessThanOrEqual(height + 1e-6);
    expect(faceMin).toBeLessThanOrEqual(Math.min(width, height));
  }
  // A bigger face draws a bigger ring, and a face too small still draws a legible one.
  expect(dialLayout(560, 482).radius).toBeGreaterThan(dialLayout(300, 320).radius);
  expect(dialLayout(0, 0).radius).toBeGreaterThan(0);
  expect(
    dialLayout(300, 320).labels.map(({ anchorX, anchorY }) => `${anchorX} ${anchorY}`),
  ).toEqual(["end start", "end end", "center end", "start end", "start start"]);
});

test("the transfer curve is monotonic, bounded and invertible, and a gigabit ring reads round labels", () => {
  const scale = 1_000;
  const knots = [0, 5, 10, 50, 100, 250, 500, 750, 1_000];
  const values = [...knots, ...knots.slice(1).map((v, i) => (v + knots[i]!) / 2)].sort(
    (a, b) => a - b,
  );
  let previous = -1;
  for (const value of values) {
    const fraction = gaugeFraction(value, scale);
    expect(fraction).toBeGreaterThanOrEqual(Math.max(0, previous));
    expect(fraction).toBeLessThanOrEqual(1);
    expect(gaugeValue(fraction, scale)).toBeCloseTo(value, 8);
    previous = fraction;
  }
  for (const odd of [Number.NaN, -5, Number.POSITIVE_INFINITY])
    expect(gaugeFraction(odd, scale)).toBe(odd > 0 ? 1 : 0);
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
  const arcs = (
    download: number | null,
    upload: number | null,
    bidi: ReturnType<typeof bidirectional> | null,
  ) =>
    resultArcs({
      download: download === null ? null : result(download),
      upload: upload === null ? null : result(upload),
      bidirectional: bidi,
    }).map(
      ({ stage, direction, bytesPerSec, dashed }) =>
        `${stage}:${direction}:${bytesPerSec}${dashed ? ":dashed" : ""}`,
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
    download: result(5),
    upload: result(50),
    bidirectional: bidirectional(60, 60),
  });
  expect(headlineArc(all)?.stage).toBe("download");
  expect(headlineArc(all.filter((arc) => arc.stage !== "download"))?.stage).toBe("upload");
  expect(headlineArc([])).toBeNull();
});

test("heads stay whole beads apart, and lie over a close higher one cut out or halved", () => {
  // On a 72 px ring with 8 px heads, 18 px apart along the ring is the stacking distance and 8 px the split one.
  expect(beadKinds([1, 0.5, 0], 72, 8)).toEqual(["bead", "bead", "bead"]);
  expect(beadKinds([0.6, 0.5, 0.46], 72, 8)).toEqual(["bead", "bead", "stacked"]);
  expect(beadKinds([0.5, 0.5, 0.49], 72, 8)).toEqual(["bead", "split", "split"]);
  expect(beadKinds([Number.NaN, 0], 72, 8)).toEqual(["bead", "split"]);
});

test("a result sweeps the rest of the ring and each bead pops as the front reaches it", () => {
  // Less ring left sweeps quicker, down to a floor that still reads as a sweep.
  expect(resultSweepMs(0.5)).toBeCloseTo(resultSweepMs(0) / 2, 6);
  expect(resultSweepMs(0.95)).toBe(resultSweepMs(1));
  expect(resultSweepMs(1)).toBeGreaterThan(0);
  expect(reachMs(0.3, 0.4, 540)).toBe(0);
  expect(reachMs(1, 0, 900)).toBe(900);
  expect(reachMs(1, 1, 360)).toBe(0);
  // With the ease-out cubic the front covers seven eighths of the way in half the time.
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
  // When the value ends it drains to zero, lit until it gets there.
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
  // A stage whose value arrives while the last stage's still drains turns the needle round, so it never jumps.
  const handing = new Needle();
  handing.update(shown("a"), 0);
  handing.angle(RISE_MS, 60);
  handing.update({ visible: false, hold: false, motion: true, course: "a" }, 1_000);
  for (let now = 1_000; now <= 1_096; now += 16) handing.angle(now, null);
  const draining = handing.angle(1_112, null);
  handing.update(shown("b"), 1_112);
  const steps: number[] = [];
  let last = draining;
  for (let now = 1_128; now <= 1_112 + RISE_MS; now += 16) {
    const angle = handing.angle(now, 240);
    steps.push(angle - last);
    last = angle;
  }
  expect(steps[0]).toBeLessThan(0);
  expect(Math.max(...steps.map(Math.abs))).toBeLessThan(15);
  expect(last).toBeCloseTo(240, 0);
  // Unseen or reduced, every change snaps and a result sweeps from zero.
  const still = new Needle();
  still.update(shown("a", false), 0);
  expect(still.angle(0, 77)).toBe(77);
  expect(still.takeover(false, 10)).toBe(0);
});

/** A grid of named servers, each with its cells by stage and its causes; stages it does not name were off. */
function judged(
  outcome: Grid["outcome"],
  servers: Record<string, { cells: Partial<Record<Stage, CellStatus>>; causes?: Partial<Cause>[] }>,
): Grid {
  const rows = Object.entries(servers).map(([name, { cells, causes = [] }]) => {
    const own = causes.map(
      (cause) => ({ serverId: name, atMs: 0, scope: "throughput", message: "", ...cause }) as Cause,
    );
    return {
      server: { id: name, name, url: "" },
      outcome,
      causes: own,
      cells: Object.fromEntries(
        STAGES.map((stage) => {
          const status = cells[stage] ?? "skipped";
          const cause =
            status === "complete" || status === "skipped"
              ? null
              : (own.find((each) => each.stage === stage) ?? own.at(-1) ?? null);
          return [stage, { status, cause }];
        }),
      ) as Grid["rows"][number]["cells"],
    };
  });
  const columns = Object.fromEntries(
    STAGES.map((stage) => {
      const all = rows.map(({ cells }) => cells[stage].status);
      const column = all.every((s) => s === "skipped")
        ? "skipped"
        : all.every((s) => s === "unreached")
          ? "unreached"
          : all.every((s) => s === "failed" || s === "unreached")
            ? "failed"
            : all.every((s) => s === "complete")
              ? "complete"
              : "partial";
      return [stage, column];
    }),
  ) as Grid["columns"];
  const measured = Object.values(columns).some((s) => s === "complete" || s === "partial");
  return { rows, columns, outcome, measured, causes: rows.flatMap((row) => row.causes) };
}

test("a run short of complete names what it lacks first, then the first cause in server order", () => {
  const lost = { stage: "download", reason: "connection-lost" } as const;
  const cases: [string, Grid, string][] = [
    [
      "one server lost in Download",
      judged("partial", {
        Frankfurt: { cells: { download: "complete", upload: "complete" } },
        Oslo: { cells: { download: "partial", upload: "unreached" }, causes: [lost] },
      }),
      "Oslo: connection lost in Download",
    ],
    [
      "dropped at Start",
      judged("partial", {
        Frankfurt: { cells: { download: "complete" } },
        Bergen: {
          cells: { download: "unreached" },
          causes: [{ stage: "start", reason: "server-capacity" }],
        },
      }),
      "Bergen: server at capacity at Start",
    ],
    [
      "loaded latency lost",
      judged("partial", {
        Frankfurt: { cells: { download: "complete" } },
        Oslo: { cells: { download: "complete" }, causes: [{ ...lost, scope: "latency" }] },
      }),
      "Oslo: connection lost in loaded download",
    ],
    [
      "two servers",
      judged("partial", {
        Frankfurt: { cells: { download: "partial" }, causes: [{ ...lost, reason: "timeout" }] },
        Oslo: { cells: { download: "partial" }, causes: [lost] },
      }),
      "2 servers with issues, each marked on its card",
    ],
    [
      "one server alone",
      judged("partial", {
        Home: {
          cells: { download: "complete", upload: "partial" },
          causes: [{ stage: "upload", reason: "insufficient-evidence" }],
        },
      }),
      "Upload: too little measured time",
    ],
    [
      "a stage without a result",
      judged("incomplete", {
        Home: {
          cells: { download: "complete", upload: "failed" },
          causes: [{ stage: "upload", reason: "connection-lost" }],
        },
      }),
      "Upload failed: connection lost",
    ],
    [
      "every server lost before a stage",
      judged("incomplete", {
        Frankfurt: { cells: { download: "partial", upload: "unreached" }, causes: [lost] },
        Oslo: { cells: { download: "partial", upload: "unreached" }, causes: [lost] },
      }),
      "Upload not reached: connection lost in Download",
    ],
    [
      "nothing measured",
      judged("incomplete", {
        Frankfurt: { cells: { download: "failed", upload: "unreached" }, causes: [lost] },
        Oslo: { cells: { download: "failed", upload: "unreached" } },
      }),
      "Frankfurt: connection lost in Download",
    ],
  ];
  for (const [name, table, hint] of cases) expect(endingHint(table), name).toBe(hint);
});
