import { expect, test } from "bun:test";
import { json } from "../../protocol/pins.testutil";
import { Population, RttEstimator } from "./latency";

const reply = (rttMs: number, timedOut = false) => ({ rttMs, timedOut, handlingMs: null, inWindow: true });

test("the shared latency vectors", async () => {
  for (const vector of await json("latency.testvectors.json")) {
    const population = new Population();
    const estimator = new RttEstimator();
    const deadlines = [estimator.deadlineMs()];
    let continuity = 0;
    for (const outcome of vector.outcomes) {
      if (outcome.break) continuity++;
      else population.observe(reply(outcome.rttMs ?? 250, !!outcome.timeout), continuity);
      if (outcome.rttMs !== undefined) {
        estimator.observe(outcome.rttMs);
        deadlines.push(estimator.deadlineMs());
      }
    }
    const summary = population.summary();
    expect(
      {
        replies: (summary?.probeCount ?? 0) - (summary?.timeoutCount ?? 0),
        timeouts: summary?.timeoutCount ?? 0,
        timeoutRatio: summary?.probeCount ? summary.timeoutCount / summary.probeCount : null,
        p50Ms: summary?.p50Ms ?? null,
        p95Ms: summary?.p95Ms ?? null,
        jitterMs: summary?.jitterMs ?? null,
        jitterPairs: summary?.jitterPairs ?? 0,
      },
      vector.name,
    ).toEqual(vector.expect);
    if (vector.deadlineMs) expect(deadlines, vector.name).toEqual(vector.deadlineMs);
  }
});

test("summaries taken while a stage runs equal one summary of the whole stage", () => {
  const rtts = [20, 5, 30, 15, 15, 7.5, 40, 1, 22, 9, 30, 5];
  const whole = new Population();
  const live = new Population();
  rtts.forEach((rttMs, index) => {
    whole.observe(reply(rttMs));
    live.observe(reply(rttMs));
    if (index % 3 === 1) live.summary();
  });
  expect(live.summary()).toEqual(whole.summary());
  expect(whole.summary()).toMatchObject({ minMs: 1, maxMs: 40, p50Ms: 15 });
});

test("only whole positive counts of lost probes count", () => {
  const population = new Population();
  for (const count of [Number.POSITIVE_INFINITY, 1.5, 2 ** 60, -1, 0]) population.interrupt(count, "send-failed");
  population.interrupt(2, "unresolved");
  expect(population.summary()).toMatchObject({ sendFailureCount: 0, unresolvedCount: 2 });
});

test("a stage past 65,536 distinct RTTs keeps its percentiles to four significant digits", () => {
  const population = new Population();
  const rtts: number[] = [];
  // Three hundred thousand replies spread over 10-110 ms, and nearly all are distinct to the nanosecond.
  for (let i = 0; i < 300_000; i++) {
    const rttMs = 10 + ((i * 7_919) % 100_000) / 1_000 + (i % 997) / 1e6;
    rtts.push(rttMs);
    population.observe(reply(rttMs));
  }
  rtts.sort((a, b) => a - b);
  const summary = population.summary()!;
  const exact = (rtts[149_999]! + rtts[150_000]!) / 2;
  expect(Math.abs(summary.p50Ms! - exact) / exact).toBeLessThan(1e-4);
});
