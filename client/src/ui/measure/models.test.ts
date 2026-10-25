import { expect, test } from "bun:test";
import type { ThroughputResult } from "../../engine/measure/aggregate";
import type { LatencyBucket } from "../../engine/measure/buckets";
import type { LatencySummary } from "../../engine/measure/latency";
import { grid, type MultiServerResult, type RunResult } from "../../engine/run/result";
import { MISSING } from "../../view/format";
import { POPULATION, reasonLabel, STAGE, STAGE_STATE } from "../../view/words";
import {
  cardLine,
  liveCard,
  serverIssues,
  settledCard,
  shownEvidence,
  type Evidence,
  type LiveEvidence,
} from "./cards-model";
import { lanesAxisMs, nearestMetric, present, stepMetric, values } from "./lanes-model";
import { Lane, monotoneTangents, replyBars } from "./strip-model";
import type { ThroughputSample } from "../../app/series";

const bucket = (startT: number, lo: number, hi: number, median = (lo + hi) / 2): LatencyBucket => ({
  phase: "latency",
  continuityId: 0,
  startT,
  endT: startT + 200,
  medianRttMs: median,
  minRttMs: lo,
  p95RttMs: hi,
  maxRttMs: hi,
  pingCount: 2,
  timeoutCount: 0,
});

test("reply bars join whole buckets to a bar's pitch, and a live bar only grows as its buckets close", () => {
  // Over 40 s on 400 px a 400 ms pitch joins pairs of 200 ms buckets on the first bucket's grid.
  const buckets = [
    bucket(1000, 10, 15),
    bucket(1200, 11, 14),
    bucket(1400, 12, 17),
    bucket(1600, 13, 16),
  ];
  const bars = (list: LatencyBucket[]) =>
    replyBars(list, "latency", 0, 40_000, 400).map(({ lo, hi, t0, t1 }) => [lo, hi, t0, t1]);
  expect(bars([...buckets, bucket(1800, 14, 18)])).toEqual([
    [10, 15, 1000, 1400],
    [12, 17, 1400, 1800],
    [14, 18, 1800, 2000],
  ]);
  expect(bars([...buckets, bucket(1800, 14, 18), bucket(2000, 15, 19)]).at(-1)).toEqual([
    14, 19, 1800, 2200,
  ]);
  // A bucket of timeouts draws nothing; another stage's buckets belong to another strip.
  const silent = { ...bucket(2200, 1, 1), medianRttMs: null };
  expect(bars([silent, { ...bucket(2400, 5, 6), phase: "download" }])).toEqual([]);
  expect(bars([bucket(1000, 10, 15), { ...bucket(1200, 11, 14), continuityId: 1 }])).toEqual([
    [10, 15, 1000, 1200],
    [11, 14, 1200, 1400],
  ]);
});

test("the strip's curve passes every bin and never overshoots a step", () => {
  const x = [0, 10, 20, 30];
  const y = [20, 20, 8, 8];
  const tangents = new Float64Array(4);
  monotoneTangents(x, y, 4, tangents);
  for (let i = 0; i < 3; i++) {
    const third = (x[i + 1]! - x[i]!) / 3;
    const controls = [y[i]! + tangents[i]! * third, y[i + 1]! - tangents[i + 1]! * third];
    for (const control of controls) {
      expect(control).toBeLessThanOrEqual(Math.max(y[i]!, y[i + 1]!));
      expect(control).toBeGreaterThanOrEqual(Math.min(y[i]!, y[i + 1]!));
    }
  }
});

test("a plot bin never averages across a recovery and its curves end at each continuity boundary", () => {
  const lane = new Lane();
  const sample = (t: number, bytesPerSec: number, continuityId: number): ThroughputSample => ({
    t,
    bytesPerSec,
    continuityId,
    bytesCumulative: 0,
    stage: "download",
    dir: "down",
  });
  lane.fill(
    [sample(10, 10, 0), sample(20, 90, 1), sample(120, 100, 1)],
    "download",
    "down",
    0,
    1_000,
    10,
  );
  expect([...lane.v.slice(0, lane.n)]).toEqual([90, 100]);
  const x = [0, 1, 2, 3];
  const y = [10, 20, 100, 90];
  const tangents = new Float64Array(4);
  monotoneTangents(x, y, 4, tangents, [0, 0, 1, 1]);
  expect([...tangents]).toEqual([10, 10, -10, -10]);
});

const summary = (
  p10: number,
  p50: number,
  p90: number,
  extra: Partial<LatencySummary> = {},
): LatencySummary => ({
  accountingComplete: true,
  probeCount: 40,
  timeoutCount: 2,
  unresolvedCount: 0,
  sendFailureCount: 0,
  jitterPairs: 30,
  minMs: p10 - 1,
  maxMs: p90 + 30,
  meanMs: p50,
  p10Ms: p10,
  p50Ms: p50,
  p90Ms: p90,
  p95Ms: p90 + 2,
  jitterMs: 1,
  ...extra,
});

test("the lanes' axis follows their P90s, and a reading walks the drawn marks along it", () => {
  expect(lanesAxisMs([values(summary(5, 8, 12)), values(summary(20, 40, 70))])).toBe(100);
  expect(lanesAxisMs([values(summary(0.2, 0.3, 0.5)), values(summary(1, 2, 3))])).toBe(4);
  expect(lanesAxisMs([values({ ...summary(5, 30, 40), p90Ms: null })])).toBe(40);
  expect(lanesAxisMs([])).toBe(20);
  // The latest reply sits where its value is, and P95 has no mark to read.
  const row = values(summary(5, 8, 12), 6);
  expect(present(row)).toEqual(["min", "p10", "current", "center", "p90", "max"]);
  expect(nearestMetric(row, 6.4)).toBe("current");
  expect(nearestMetric(row, 13.9)).toBe("p90");
  expect(nearestMetric(values(null), 3)).toBeNull();
  expect(stepMetric(row, "current", "ArrowRight")).toBe("center");
  expect(stepMetric(row, null, "ArrowLeft")).toBe("max");
  expect(stepMetric(row, "min", "ArrowLeft")).toBe("min");
  expect(stepMetric(row, "p10", "End")).toBe("max");
});

const BITS = { base: "base10", kind: "bits", tier: 2 } as const;
const transfer = (bytesPerSec: number): ThroughputResult => ({
  peakBytesPerSec: bytesPerSec * 1.1,
  stabilityPct: 91.7,
  totalBytes: 373_800_000,
  reportedBytesPerSec: bytesPerSec,
});
const NOTHING: Evidence = {
  status: {},
  whole: new Set(),
  download: null,
  upload: null,
  bidirectional: null,
  latency: null,
  idle: null,
};

test("a card keeps the same facts from Start to its result, and a one-direction pair keeps its unit", () => {
  const facts = (card: ReturnType<typeof settledCard>) =>
    card.facts.map(({ key, value }) => `${key} ${value}`);
  const download = settledCard(
    "download",
    "partial",
    { ...NOTHING, download: transfer(50e6) },
    BITS,
    true,
  );
  expect([download.figure, download.unit, download.wire]).toEqual(["400.0", "Mbit/s", null]);
  expect(facts(download)).toEqual([
    "peak 440.0 Mbit/s",
    `stability ${MISSING}`,
    "transferred 373.8 MB",
  ]);
  const pending = liveCard("download", "upcoming", { own: false } as LiveEvidence, BITS);
  expect(facts(pending)).toEqual([
    `peak ${MISSING}`,
    `stability ${MISSING}`,
    `transferred ${MISSING}`,
  ]);
  expect([pending.status, pending.quiet, pending.word]).toEqual(["pending", true, null]);
  const lanes = { down: transfer(5e6), up: null, wire: null };
  const oneWay = settledCard(
    "bidirectional",
    "failed",
    { ...NOTHING, bidirectional: lanes },
    BITS,
    true,
  );
  expect([oneWay.figure, oneWay.pair, oneWay.unit]).toEqual([
    MISSING,
    { down: "40.00", up: MISSING },
    "Mbit/s",
  ]);
  const latency = settledCard(
    "latency",
    "complete",
    {
      ...NOTHING,
      latency: { reportedMs: 12.34, jitterMs: 0.8, stabilityPct: 93.6 },
      idle: summary(9.6, 12, 31.2),
    },
    BITS,
    true,
  );
  expect([latency.figure, latency.unit, latency.jitter]).toEqual(["12.3", "ms", "0.8 ms"]);
  expect(facts(latency)).toEqual([
    "stability 93.6%",
    "range 8.6 to 61.2 ms",
    "replies 38",
    "timeouts 5.0%",
  ]);
});

test("a running card follows its own stage only, says when replies stop, and a stopped one keeps its last value", () => {
  const evidence = (extra: Partial<LiveEvidence>): LiveEvidence => ({
    own: true,
    lost: false,
    spoken: null,
    held: null,
    idle: null,
    bytes: 1_000_000,
    shape: null,
    ...extra,
  });
  const warming = liveCard("upload", "warmup", evidence({ own: false }), BITS);
  expect([warming.status, warming.word, warming.figure, warming.live]).toEqual([
    "active",
    STAGE_STATE.warmup,
    MISSING,
    null,
  ]);
  const running = liveCard(
    "upload",
    "running",
    evidence({ spoken: 12.5e6, shape: { peak: 2e6, stability: null } }),
    BITS,
  );
  expect([running.live, running.unit, running.spoken]).toEqual(["rate", "Mbit/s", "100.0 Mbit/s"]);
  expect(running.facts.map(({ value }) => value)).toEqual(["16.00 Mbit/s", MISSING, "1.0 MB"]);
  // Unanswered probes leave no figure to follow and are spoken in its place.
  const silent = liveCard("latency", "running", evidence({ lost: true }), BITS);
  expect([silent.figure, silent.live, !!silent.spoken]).toEqual([MISSING, null, true]);
  const stopped = liveCard("download", "stopped", evidence({ own: false, held: 25e6 }), BITS);
  expect([stopped.status, stopped.figure, stopped.word]).toEqual([
    "stopped",
    "200.0",
    STAGE_STATE.stopped,
  ]);
  const pair = liveCard("bidirectional", "recovering", evidence({}), BITS);
  expect([pair.status, pair.live, pair.word]).toEqual([
    "recovering",
    "pair",
    STAGE_STATE.recovering,
  ]);
});

test("a server's issues name the stages each reason ended", () => {
  const failure = (stage: string, scope: string, reason: string, serverId = "a") => ({
    serverId,
    stage,
    scope,
    reason,
  });
  const details = {
    selection: [{ id: "a", name: "Oslo" }],
    failures: [
      failure("download", "throughput", "timeout"),
      failure("upload", "latency", "timeout"),
      failure("latency", "latency", "connection-lost"),
      failure("upload", "throughput", "timeout", "b"),
    ],
  } as unknown as MultiServerResult;
  expect(serverIssues(details, "a")).toEqual([
    {
      server: "Oslo",
      stages: `${STAGE.download.label}, ${POPULATION.upload}`,
      reason: reasonLabel("timeout"),
      throughput: ["download"],
    },
    {
      server: "Oslo",
      stages: STAGE.latency.label,
      reason: reasonLabel("connection-lost"),
      throughput: [],
    },
  ]);
});

test("shown together, the latency card reads the server latency is shown for, headline and spread alike", () => {
  const summary = (p50Ms: number, probeCount: number) =>
    ({ p50Ms, minMs: p50Ms, maxMs: p50Ms, probeCount }) as LatencySummary;
  const entry = (id: string, p50Ms: number, probes: number) => ({
    server: { id },
    stages: { latency: "complete" },
    latency: { medianMs: p50Ms },
    latencyByStage: { latency: summary(p50Ms, probes) },
  });
  const [near, far] = [entry("near", 16, 12), entry("far", 121, 9)];
  const details = { servers: [near, far] } as unknown as MultiServerResult;
  const run = {
    status: { latency: "complete" as const, download: "complete" as const },
    latency: { medianMs: 16 },
    download: null,
    upload: null,
    bidirectional: null,
  } as unknown as Omit<Evidence, "idle" | "whole">;
  const result = {
    ...run,
    stages: {
      latency: "complete",
      download: "not-run",
      upload: "not-run",
      bidirectional: "not-run",
    },
    multiServer: { ...details, selection: [near.server, far.server], failures: [] },
  } as unknown as RunResult;
  const live = summary(121, 40);
  const shown = (focus: string, lens = "") =>
    shownEvidence({ result: lens ? result : null, details, lens, focus, run, idle: live });
  // Latency shown for the far server reads its result and its own replies, never the run's near headline.
  expect([shown("far").latency, shown("far").idle?.probeCount]).toEqual([far.latency as never, 9]);
  expect(shown("far").status).toEqual({ latency: "complete", download: "complete" });
  expect([shown("near").latency, shown("near").idle?.probeCount]).toEqual([
    near.latency as never,
    12,
  ]);
  // A lens on one server reads that server alone; with no result yet, the run's own and the live replies.
  expect(shown("near", "far").latency).toBe(far.latency as never);
  expect(
    shownEvidence({ result: null, details: null, lens: "", focus: "far", run, idle: live }).idle,
  ).toBe(live);
});

test("a card's line names who failed in its stage and who never reached it; one server shown alone says why", () => {
  const frankfurt = { id: "f", name: "Frankfurt", url: "" };
  const oslo = { id: "o", name: "Oslo", url: "" };
  const bergen = { id: "b", name: "Bergen", url: "" };
  const all = {
    latency: "not-run",
    download: "complete",
    upload: "complete",
    bidirectional: "not-run",
  };
  const failure = (serverId: string, reason: string) => ({
    serverId,
    stage: "download",
    atMs: 0,
    scope: "throughput",
    reason,
    message: "",
  });
  const table = grid({
    latency: null,
    download: transfer(1e6),
    upload: transfer(1e6),
    bidirectional: null,
    stages: { ...all, download: "partial", upload: "partial" },
    multiServer: {
      selection: [frankfurt, oslo, bergen],
      servers: [
        { server: frankfurt, stages: all },
        { server: oslo, stages: { ...all, download: "partial", upload: "not-run" } },
      ],
      failures: [failure("o", "connection-lost"), failure("b", "server-capacity")],
    },
  } as unknown as RunResult);
  expect(cardLine(table, "download")).toBe("Oslo: connection lost, without Bergen");
  expect(cardLine(table, "upload")).toBe("Without Oslo and Bergen");
  expect(cardLine(table, "download", "o")).toBe("Connection lost");
  expect(cardLine(table, "upload", "o")).toBe("Left in Download: connection lost");
  expect(cardLine(table, "upload", "b")).toBe("Left at Start: server at capacity");
  expect(cardLine(table, "upload", "f")).toBe("");
});
