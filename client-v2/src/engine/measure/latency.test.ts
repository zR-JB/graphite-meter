import { expect, test } from "bun:test";
import { json } from "../../protocol/pins.testutil";
import { Population, RttEstimator } from "./latency";

test("the shared latency vectors", async () => {
  for (const vector of await json("latency.testvectors.json")) {
    const population = new Population();
    const estimator = new RttEstimator();
    const deadlines = [estimator.deadlineMs()];
    let continuity = 0;
    for (const outcome of vector.outcomes) {
      if (outcome.break) continuity++;
      else population.observe({ rttMs: outcome.rttMs ?? 250, timedOut: !!outcome.timeout }, continuity);
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
        timeoutRatio: population.timeoutRatio,
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
    whole.observe({ rttMs, timedOut: false });
    live.observe({ rttMs, timedOut: false });
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
