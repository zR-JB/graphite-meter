import { link, openApp, reached, runConfig, shapedFleet, start } from "./harness";
import { expect, test } from "../webview";

test(
  "a server lost mid-upload is reported while the remaining servers continue",
  async (page) => {
    const fleet = await shapedFleet([20, 20, 20, 20].map((rtt) => link(rtt, 100)));
    try {
      await openApp(
        page,
        fleet,
        runConfig("protocol:http1", {
          transports: {
            throughputTarget: "protocol:http1",
            latencyTarget: "transport:webtransport",
          },
          duration: {
            warmupMs: 500,
            latencyMs: 1000,
            downloadMs: 3000,
            uploadMs: 8000,
            bidirectionalMs: 2000,
          },
        }),
      );
      const running = await start(page, fleet);
      await reached(page, "upload");
      await Bun.sleep(1500);
      fleet.children[2]!.kill("SIGKILL");
      const { record } = await running.end();
      expect(record).toBeDefined();
      const { failures, servers } = record!.result.multiServer;
      const failure = failures.find(
        ({ serverId, stage }) => serverId === "s3" && stage === "upload",
      );
      expect(failure?.reason).toBe("connection-lost");
      for (const server of servers.filter(({ server }) => server.id !== "s3"))
        expect(server.upload?.reportedBytesPerSec).toBeGreaterThan(0);
    } finally {
      await fleet.close();
    }
  },
  { timeout: 120_000 },
);
