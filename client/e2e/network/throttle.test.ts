// Section 2.1 in Chrome, which delays every WebTransport handshake in a page after failures for 5 minutes; a reload
// resets it.
import {
  attempt,
  capped,
  link,
  measured,
  openApp,
  reached,
  runConfig,
  shapedFleet,
  start,
} from "./harness";
import { expect, test } from "../webview";

const HANDLERS = 8;

test.chrome(
  "T9: a server refusing WebTransport for 15 s costs at most 4 failed handshakes, named, and a reload recovers",
  async (page) => {
    // Once the run has its latency bus, another client's downloads take every other handler, so the server refuses
    // the run's sessions until that client leaves.
    const fleet = await shapedFleet([link(20, 200)], { env: [capped(HANDLERS)] });
    try {
      await openApp(
        page,
        fleet,
        runConfig("transport:webtransport", {
          transports: {
            throughputTarget: "transport:webtransport",
            latencyTarget: "transport:websocket",
          },
        }),
      );
      const from = fleet.logs[0]!.mark();
      const refused = await start(page, fleet);
      await reached(page, "latency");
      fleet.neighbour(`${fleet.servers[0]!.http}/download?bytes=${2 ** 40}`, {
        requests: HANDLERS,
        everyMs: 50,
      });
      await Bun.sleep(15_000);
      await fleet.stopNeighbours();
      const run = await refused.end();
      const dials = fleet.logs[0]!.lines(from).flatMap(
        (line) => /request:\s+CONNECT \/wt\/\S+ .*: (\d+)$/.exec(line)?.[1] ?? [],
      );
      const failed = dials.filter((status) => status !== "200").length;
      expect(failed).toBeGreaterThan(0);
      expect(failed).toBeLessThanOrEqual(4);
      expect(dials).not.toContain("429");
      expect(measured(run).outcome).not.toBe("complete");
      // The page stops dialing WebTransport once its cap is spent, and says why and what resets it.
      const paused = await attempt(page, fleet);
      expect(`${paused.notice} ${measured(paused).failures.join(" ")}`).toContain("Reload");
      await page.reload();
      expect(measured(await attempt(page, fleet))).toEqual({ outcome: "complete", failures: [] });
    } finally {
      await fleet.close();
    }
  },
  {
    timeout: 180_000,
    failing:
      "until Start names the page's WebTransport cap (spec 7.3): a paused page's Start says only that its path is unavailable",
  },
);
