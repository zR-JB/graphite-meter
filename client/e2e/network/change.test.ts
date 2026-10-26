// A link that slows in the middle of each transfer stage: the run keeps each drop with its time and both rates, the
// card says it, and the headline stays its interval's mean.
import { link, measured, openApp, reached, runConfig, shapedFleet, start } from "./harness";
import { expect, test } from "../webview";

const FAST = link(20, 80);
const SLOW = link(20, 30);
const SLOWS_AT_MS = 3_500;
/** Payload bytes per second of a link's rate, less what TCP and TLS add. */
const payload = (mbit: number) => (mbit * 1e6 * 0.93) / 8;

test(
  "T11: a link that slows mid-stage records the drop with its time and rates, and the card says it",
  async (page) => {
    const fleet = await shapedFleet([FAST]);
    try {
      await openApp(
        page,
        fleet,
        runConfig("auto", {
          stages: { latency: false, download: true, upload: true, bidirectional: false },
          duration: {
            warmupMs: 800,
            latencyMs: 2000,
            downloadMs: 8000,
            uploadMs: 8000,
            bidirectionalMs: 3000,
          },
          adaptive: false,
        }),
      );
      const running = await start(page, fleet);
      for (const stage of ["download", "upload"]) {
        await reached(page, stage);
        await Bun.sleep(SLOWS_AT_MS);
        fleet.reshape(0, SLOW);
        if (stage === "download") {
          await reached(page, "warmup");
          fleet.reshape(0, FAST);
        }
      }
      const run = await running.end();
      expect(measured(run)).toEqual({ outcome: "complete", failures: [] });
      const [server] = run.record!.result.multiServer.servers;
      for (const stage of ["download", "upload"] as const) {
        const own = server![stage]!;
        const [change, ...more] = own.changes ?? [];
        expect(more).toEqual([]);
        // The live rate reads a drop within its 750 ms fast window, after the page saw the stage begin.
        expect(change!.atMs).toBeGreaterThan(SLOWS_AT_MS - 500);
        expect(change!.atMs).toBeLessThan(SLOWS_AT_MS + 3_000);
        expect(change!.fromBytesPerSec / payload(80)).toBeWithin(0.8, 1.15);
        expect(change!.toBytesPerSec / payload(30)).toBeWithin(0.8, 1.15);
        // The headline rule is unchanged: one interval, both regimes in its mean.
        expect(own.reportedBytesPerSec).toBeWithin(change!.toBytesPerSec, change!.fromBytesPerSec);
      }
      for (const stage of ["download", "upload"])
        await expect(page.locator(`.card[data-tone="${stage}"] .line .change`)).toHaveText(
          /Dropped from \d+\.\d+ to \d+\.\d+ Mbit\/s at \d\.\d s/,
        );
    } finally {
      await fleet.close();
    }
  },
  { timeout: 120_000 },
);
