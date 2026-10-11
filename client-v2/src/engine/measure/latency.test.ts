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
