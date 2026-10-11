import { expect, test } from "bun:test";
import { json } from "../../protocol/pins.testutil";
import { RateEstimator } from "./live";

test("the live rate the terminal clients share", async () => {
  for (const { name, steps } of (await json("liverate.testvectors.json")).cases) {
    const estimator = new RateEstimator();
    for (const [bytes, ms, presented, changed] of steps) {
      expect(estimator.observe(bytes, ms), name).toBe(changed);
      expect(estimator.presented, name).toBe(presented);
    }
  }
});
