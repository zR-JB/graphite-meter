import { expect, test } from "bun:test";
import { LatencyAccumulator } from "./latencySummary";
import { RunAccumulator } from "./evaluation";
import { DEFAULT_CONFIG } from "../state/defaults";

test("latency summaries preserve the raw distribution and consecutive RTT variation", () => {
  const stats = new LatencyAccumulator();
  for (const rtt of [10, 100, 10, 100]) stats.observe(rtt, false, 0);
  expect(stats.snapshot()).toEqual({
    accountingComplete: true,
    probeCount: 4,
    timeoutCount: 0,
    unresolvedCount: 0,
    sendFailureCount: 0,
    jitterPairs: 3,
    minMs: 10,
    maxMs: 100,
    meanMs: 55,
    p10Ms: 10,
    p50Ms: 55,
    p90Ms: 100,
    p95Ms: 100,
    jitterMs: 90,
  });
  const captured = stats.snapshot();
  stats.observe(1_000, false, 0);
  expect(captured?.maxMs).toBe(100);
});

test("unknown worker outcomes preserve incompleteness without invented counts", () => {
  const stats = new LatencyAccumulator();
  stats.markAccountingIncomplete();
  expect(stats.snapshot()).toMatchObject({
    accountingComplete: false,
    probeCount: 0,
    timeoutCount: 0,
    unresolvedCount: 0,
    sendFailureCount: 0,
  });
  expect(stats.probeTimeoutPct).toBeNull();
  stats.observe(10, false, 0);
  expect(stats.snapshot()?.accountingComplete).toBe(false);
});

test("timeouts have no RTT and interrupted probes have no timeout verdict", () => {
  const stats = new LatencyAccumulator();
  expect(stats.snapshot()).toBeNull();
  expect(stats.probeTimeoutPct).toBeNull();
  stats.interrupt(2, "unresolved");
  stats.interrupt(1, "send-failed");
  expect(stats.probeTimeoutPct).toBeNull();
  expect(stats.snapshot()).toMatchObject({
    probeCount: 0,
    unresolvedCount: 2,
    sendFailureCount: 1,
    jitterMs: null,
  });
  stats.observe(10_000, true, 0);
  expect(stats.probeTimeoutPct).toBe(100);
  expect(stats.snapshot()).toMatchObject({
    probeCount: 1,
    timeoutCount: 1,
    minMs: null,
    p50Ms: null,
  });
  stats.observe(0, false, 0);
  expect(stats.probeTimeoutPct).toBe(50);
  expect(stats.snapshot()?.minMs).toBe(0);
});

test("jitter skips timeouts but never bridges an interrupted connection", () => {
  const stats = new LatencyAccumulator();
  stats.observe(10, false, 0);
  stats.observe(250, true, 0);
  stats.observe(30, false, 0);
  stats.observe(1_000, false, 1);
  stats.observe(1_010, false, 1);
  expect(stats.snapshot()?.jitterMs).toBe(15);
  expect(stats.snapshot()?.jitterPairs).toBe(2);
});

test("post-cutoff replies resolve probes without contaminating measured RTTs", () => {
  const stats = new LatencyAccumulator();
  stats.observe(10, false, 0);
  stats.observe(900, false, 0, false);
  stats.observe(1_000, true, 0);
  expect(stats.snapshot()).toMatchObject({
    probeCount: 3,
    timeoutCount: 1,
    meanMs: 10,
    maxMs: 10,
    jitterMs: null,
  });
  expect(stats.probeTimeoutPct).toBeCloseTo(100 / 3);
});

test("stage statistics stay separate and the worst loaded median sets added latency", () => {
  const run = new RunAccumulator();
  for (const rtt of [10, 20]) run.pushLatency("latency", rtt, false);
  for (let i = 0; i < 100; i++) run.pushLatency("download", 20, false);
  run.pushLatency("upload", 300, false);
  run.pushLatency("upload", 250, true);
  run.pushThroughput("download", "down", 100, 1);
  run.pushThroughput("upload", "up", 100, 1);
  expect(run.throughputResult("download", false).probeTimeoutPct).toBe(0);
  expect(run.throughputResult("upload", false).probeTimeoutPct).toBe(50);
  expect(run.latencyResult(DEFAULT_CONFIG)).toMatchObject({
    minMs: 10,
    p50Ms: 15,
    p95Ms: 20,
  });
  expect(run.bufferbloatGrade()).toEqual({
    grade: "F",
    idleMs: 15,
    loadedMs: 300,
    increaseMs: 285,
  });
  expect(run.latencySummary("bidirectional")).toBeNull();
});

test("no measured RTT is never replaced by a preflight hint or loaded result", () => {
  const run = new RunAccumulator();
  run.pushLatency("download", 30, false);
  expect(run.latencyResult(DEFAULT_CONFIG)).toBeNull();
  expect(run.throughputResult("upload", false).probeTimeoutPct).toBeNull();
});
