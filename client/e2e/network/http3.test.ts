// Section 2.4: HTTP/3 after an idle gap, and an HTTP/3 upload after a download, signed in, so lanes are preflighted.
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

const pages: Attempt[][] = [];

// Chrome reaches these servers' HTTP/3 only by its forced-QUIC flag; its Alt-Svc racing and broken-alternative
// marking are not covered, since no public test instance is available.
test(
  "T3: an HTTP/3 run started 20 s after its check carries every lane over HTTP/3",
  async (page) => {
    const fleet = await shapedFleet([link(100, 100)], { signIn: true });
    try {
      await openApp(page, fleet, runConfig("protocol:http3"));
      await Bun.sleep(20_000);
      const run = await attempt(page, fleet);
      pages.push([run]);
      expect(measured(run)).toEqual({ outcome: "complete", failures: [] });
      expect(run.record!.result.multiServer.servers[0]!.throughput.browserProtocol).toBe("h3");
      expect(run.servers[0]!.downloads).toBeGreaterThan(0);
      expect(run.servers[0]!.companionLanes).toBe(0);
    } finally {
      await fleet.close();
    }
  },
  { timeout: 120_000 },
);

// The link is what the run's own download reached over it, so a slow runner moves both directions alike.
test.chrome(
  "T8: an HTTP/3 upload after the download fills 0.9 of a 20 ms link",
  async (page) => {
    const fleet = await shapedFleet([link(20, 200)], { signIn: true });
    try {
      await openApp(
        page,
        fleet,
        runConfig("protocol:http3", {
          stages: { latency: false, download: true, upload: true, bidirectional: false },
          duration: {
            warmupMs: 800,
            latencyMs: 2000,
            downloadMs: 5000,
            uploadMs: 5000,
            bidirectionalMs: 5000,
          },
        }),
      );
      const run = await attempt(page, fleet);
      pages.push([run]);
      expect(measured(run)).toEqual({ outcome: "complete", failures: [] });
      const { download, upload } = run.record!.result.multiServer.servers[0]!;
      expect(upload!.reportedBytesPerSec).toBeGreaterThanOrEqual(
        0.9 * download!.reportedBytesPerSec,
      );
    } finally {
      await fleet.close();
    }
  },
  {
    timeout: 120_000,
    failing:
      "until step 11: the upload reuses the download's QUIC connection, whose Cubic left slow start",
  },
);

budgetTest("the HTTP/3 runs", pages);
