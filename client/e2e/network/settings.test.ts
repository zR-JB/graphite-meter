// Section 7.9: settings that cannot apply to a running test apply to the next one.
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
import { choosePath, closeSettings, openSettings } from "../fleet";
import { expect, test } from "../webview";

test(
  "T10: a path and stream count changed during a run apply to the next run",
  async (page) => {
    const fleet = await shapedFleet([link(20, 100)]);
    try {
      await openApp(
        page,
        fleet,
        runConfig("protocol:http1", {
          stages: { latency: false, download: true, upload: true, bidirectional: false },
          duration: {
            warmupMs: 500,
            latencyMs: 1000,
            downloadMs: 6000,
            uploadMs: 2000,
            bidirectionalMs: 2000,
          },
          transferStreams: { mode: "forced", count: 2 },
        }),
      );
      const running = await start(page, fleet);
      await reached(page, "download");
      const settings = await openSettings(page);
      await choosePath(settings, "Throughput path", /^HTTP\/2/);
      await settings.getByRole("button", { name: "Streams per direction, increase" }).click();
      await expect(settings).toContainText("Applies to the next test");
      await closeSettings(page);
      expect(measured(await running.end()).outcome).toBe("complete");
      const next = await attempt(page, fleet);
      expect(measured(next)).toEqual({ outcome: "complete", failures: [] });
      expect(next.record!.result.multiServer.servers[0]!.throughput.protocol).toBe("http2");
      expect(
        await page.evaluate(
          () => JSON.parse(localStorage.getItem("graphite-meter:v1")!).config.transferStreams,
        ),
      ).toEqual({ mode: "forced", count: 3 });
      expect(next.servers[0]!.downloads).toBeGreaterThanOrEqual(3);
    } finally {
      await fleet.close();
    }
  },
  { timeout: 120_000 },
);
