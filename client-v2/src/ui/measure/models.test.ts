import { expect, test } from "bun:test";
import type { ThroughputResult } from "../../engine/measure/aggregate";
import type { LatencyBucket } from "../../engine/measure/buckets";
import type { LatencySummary } from "../../engine/measure/latency";
import type { MultiServerResult } from "../../engine/run/result";
import { liveCard, serverIssues, settledCard, type Evidence, type LiveEvidence } from "./cards-model";
import { lanesAxisMs, nearestMetric, present, stepMetric, values } from "./lanes-model";
import { monotoneTangents, replyBars } from "./strip-model";

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
  // 40 s over 400 px: a 400 ms pitch joins pairs of 200 ms buckets on the first bucket's grid.
  const buckets = [bucket(1000, 10, 15), bucket(1200, 11, 14), bucket(1400, 12, 17), bucket(1600, 13, 16)];
  const bars = (list: LatencyBucket[]) =>
    replyBars(list, "latency", 0, 40_000, 400).map(({ lo, hi, t0, t1 }) => [lo, hi, t0, t1]);
  expect(bars([...buckets, bucket(1800, 14, 18)])).toEqual([
    [10, 15, 1000, 1400],
    [12, 17, 1400, 1800],
    [14, 18, 1800, 2000],
  ]);
  expect(bars([...buckets, bucket(1800, 14, 18), bucket(2000, 15, 19)]).at(-1)).toEqual([14, 19, 1800, 2200]);
  // A bucket of timeouts draws nothing; another stage's buckets belong to another strip.
  const silent = { ...bucket(2200, 1, 1), medianRttMs: null };
  expect(bars([silent, { ...bucket(2400, 5, 6), phase: "download" }])).toEqual([]);
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

const summary = (p10: number, p50: number, p90: number, extra: Partial<LatencySummary> = {}): LatencySummary => ({
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
const NOTHING: Evidence = { status: {}, download: null, upload: null, bidirectional: null, latency: null, idle: null };

test("a card keeps the same facts from Start to its result, and a one-direction pair keeps its unit", () => {
  const facts = (card: ReturnType<typeof settledCard>) => card.facts.map(({ label, value }) => `${label} ${value}`);
  const download = settledCard("download", "partial", { ...NOTHING, download: transfer(50e6) }, BITS, true);
  expect([download.figure, download.unit, download.wire]).toEqual(["400.0", "Mbit/s", null]);
  expect(facts(download)).toEqual(["Peak 440.0 Mbit/s", "Stability —", "Transferred 373.8 MB"]);
  const pending = liveCard("download", "upcoming", { own: false } as LiveEvidence, BITS);
  expect(facts(pending)).toEqual(["Peak —", "Stability —", "Transferred —"]);
  expect([pending.status, pending.quiet, pending.word]).toEqual(["pending", true, null]);
  const lanes = { down: transfer(5e6), up: null, wire: null };
  const oneWay = settledCard("bidirectional", "failed", { ...NOTHING, bidirectional: lanes }, BITS, true);
  expect([oneWay.figure, oneWay.pair, oneWay.unit]).toEqual(["—", { down: "40.00", up: "—" }, "Mbit/s"]);
  const latency = settledCard(
    "latency",
    "complete",
    { ...NOTHING, latency: { reportedMs: 12.34, jitterMs: 0.8, stabilityPct: 93.6 }, idle: summary(9.6, 12, 31.2) },
    BITS,
    true,
  );
  expect([latency.figure, latency.unit, latency.jitter]).toEqual(["12.3", "ms", "0.8 ms"]);
  expect(facts(latency)).toEqual(["Stability 94%", "Range 8.6–61.2 ms", "Replies 38", "Timeouts 5.0%"]);
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
  expect([warming.status, warming.word?.label, warming.figure, warming.live]).toEqual(["active", "Running", "—", null]);
  const running = liveCard(
    "upload",
    "running",
    evidence({ spoken: 12.5e6, shape: { peak: 2e6, stability: null } }),
    BITS,
  );
  expect([running.live, running.unit, running.spoken]).toEqual(["rate", "Mbit/s", "100.0 Mbit/s"]);
  expect(running.facts.map(({ value }) => value)).toEqual(["16.00 Mbit/s", "—", "1.0 MB"]);
  const silent = liveCard("latency", "running", evidence({ lost: true }), BITS);
  expect([silent.figure, silent.unit, silent.live, silent.spoken]).toEqual(["—", "timeout", null, "probe timeout"]);
  const stopped = liveCard("download", "stopped", evidence({ own: false, held: 25e6 }), BITS);
  expect([stopped.status, stopped.figure, stopped.word?.label]).toEqual(["stopped", "200.0", "Stopped"]);
  const pair = liveCard("bidirectional", "recovering", evidence({}), BITS);
  expect([pair.status, pair.live, pair.word?.label]).toEqual(["recovering", "pair", "Recovering"]);
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
    { server: "Oslo", stages: "Download, Loaded up", reason: "Stopped delivering data", throughput: ["download"] },
    { server: "Oslo", stages: "Latency", reason: "Connection lost", throughput: [] },
  ]);
});
