// A server lost for good in the middle of a four-server run.
import { link, openApp, reached, runConfig, shapedFleet, start } from "./harness";
import { expect, test } from "../webview";

test(
  "T5: a server lost mid-download fails that stage as a lost connection, and the others complete",
  async (page) => {
    const fleet = await shapedFleet([20, 20, 20, 20].map((rtt) => link(rtt, 100)));
    try {
      await openApp(
        page,
        fleet,
        runConfig("auto", {
          duration: {
            warmupMs: 500,
            latencyMs: 1000,
            downloadMs: 4000,
            uploadMs: 2000,
            bidirectionalMs: 2000,
          },
        }),
      );
      const running = await start(page, fleet);
      await reached(page, "download");
      await Bun.sleep(1500);
      fleet.children[2]!.kill("SIGKILL");
      const { record } = await running.end();
      const { failures, servers } = record!.result.multiServer;
      // Throughput and loaded latency each fail on their own.
      const named = failures.map(({ serverId, stage, reason }) => `${serverId} ${stage} ${reason}`);
      expect(new Set(named)).toEqual(new Set(["s3 download connection-lost"]));
      const others = servers.filter(({ server }) => server.id !== "s3");
      expect(others.map(({ server }) => server.id)).toEqual(["self", "s2", "s4"]);
      for (const { download, upload } of others)
        expect(
          Math.min(download!.reportedBytesPerSec, upload!.reportedBytesPerSec),
        ).toBeGreaterThan(0);
    } finally {
      await fleet.close();
    }
  },
  { timeout: 120_000 },
);
