// Section 2.3: four servers at 10, 40, 90 and 160 ms RTT, each 100 Mbit/s with one 1000-packet queue per direction.
import {
  attempt,
  budgetTest,
  link,
  measured,
  openApp,
  runConfig,
  shapedFleet,
  type Attempt,
} from "./harness";
import { expect, test } from "../webview";

const STREAMS = 4;

/** T4 over each of `paths`, named and targeted, then T7 over their runs. */
export function distanceTests(paths: (readonly [string, string])[]) {
  const pages: Attempt[][] = [];
  for (const [name, target] of paths)
    test(
      `T4: four servers at 10 to 160 ms RTT measure every stage of five ${name} runs`,
      async (page) => {
        const fleet = await shapedFleet([10, 40, 90, 160].map((rtt) => link(rtt, 100)));
        try {
          await openApp(
            page,
            fleet,
            runConfig(target, {
              duration: {
                warmupMs: 500,
                latencyMs: 1000,
                downloadMs: 2000,
                uploadMs: 2000,
                bidirectionalMs: 2000,
              },
              transferStreams: { mode: "forced", count: STREAMS },
            }),
          );
          const runs: Attempt[] = [];
          pages.push(runs);
          for (let i = 0; i < 5; i++) runs.push(await attempt(page, fleet));
          expect(runs.map(measured)).toEqual(
            runs.map(() => ({ outcome: "complete", failures: [] })),
          );
        } finally {
          await fleet.close();
        }
      },
      { timeout: 300_000 },
    );
  // A server's lanes have one URL per transfer stage, which a cached preflight covers.
  budgetTest(`the ${paths.map(([name]) => name).join(", ")} runs`, pages, 2 * STREAMS);
}
