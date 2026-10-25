// Section 2.1 in Chrome, which delays every WebTransport handshake in a page after failures for 5 minutes; a reload
// resets it.
import {
  attempt,
  link,
  measured,
  openApp,
  reached,
  runConfig,
  shapedFleet,
  start,
} from "./harness";
import { ready } from "../fleet";
import { expect, test } from "../webview";

const OUTAGE_MS = 15_000;

test.chrome(
  "T9: WebTransport lost mid-run costs the page at most 4 failed handshakes, named, and a reload recovers",
  async (page) => {
    // A lease holds the run's sessions and Start proves the origin, so only UDP lost after Start fails the dials.
    const fleet = await shapedFleet([link(20, 200)]);
    const [server] = fleet.servers;
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
      // The workers' and the page's reports of failed handshakes; the cap also counts dials it abandons unreported.
      const failed = () => page.evaluate<number>("window.__gmFailedHandshakes");
      for (let round = 0; round < 3 && (await failed()) < 4; round++) {
        const running = await start(page, fleet);
        await reached(page, "latency");
        await fleet.udpOutage(server!, OUTAGE_MS);
        expect(measured(await running.end()).outcome).not.toBe("complete");
        expect(await failed()).toBeLessThanOrEqual(4);
      }
      expect(await failed()).toBe(4);
      const dials = () =>
        fleet.logs[0]!.lines(from).flatMap(
          (line) => /request:\s+CONNECT \/wt\/\S+ .*: (\d+)$/.exec(line)?.[1] ?? [],
        );
      // The page dials WebTransport no more once its cap is spent, and says why and what resets it.
      const before = dials().length;
      const paused = await attempt(page, fleet);
      expect(`${paused.notice} ${measured(paused).failures.join(" ")}`).toContain("Reload");
      expect(dials().length).toBe(before);
      await page.reload();
      await ready(page, 30_000);
      expect(measured(await attempt(page, fleet))).toEqual({ outcome: "complete", failures: [] });
      // Every dial that reached the server was admitted under the lease.
      expect(dials().length).toBeGreaterThan(before);
      expect(dials().filter((status) => status !== "200")).toEqual([]);
    } finally {
      await fleet.close();
    }
  },
  { timeout: 240_000 },
);
