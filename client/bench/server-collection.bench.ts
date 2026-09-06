// Drives the shipped UI and coordinator through the isolated Linux network rig.
import { test } from "bun:test";
import {
  Page,
  openSettings,
  expect,
  startTest,
  waitForCompletion,
} from "../browser/webview";
import { HISTORY_DB } from "../src/lib/history/dbSchema";
import type { HistoryRecord } from "../src/lib/history/types";

const servers: { id: string; url: string; name: string }[] = JSON.parse(
  process.env.GM_MULTI_BENCH_SERVERS!,
);
const count = Number(process.env.GM_MULTI_BENCH_COUNT);
if (![1, 2, 4].includes(count))
  throw new Error("Select a 1-, 2-, or 4-server cell");
test("coordinated server collection cell", async () => {
  const page = new Page();
  try {
    console.log("GM_BENCH_STEP init");
    await page.addInitScript(
      ({ servers, count }) => {
        window.addEventListener("graphite-meter-history-changed", () => {
          (window as any).__gmHistorySaved = true;
        });
        localStorage.setItem(
          "graphite-meter:server-selection:v1",
          JSON.stringify(
            servers
              .slice(0, count)
              .map(({ id, url }: { id: string; url: string }) => ({ id, url })),
          ),
        );
        localStorage.setItem(
          "graphite-meter:v1",
          JSON.stringify({
            resultHistoryPreference: "enabled",
            config: {
              transports: {
                throughputTarget: "protocol:http1",
                latencyTarget: "transport:websocket",
              },
              stages: {
                latency: true,
                download: true,
                upload: false,
                bidirectional: false,
              },
              duration: {
                warmupMs: 750,
                latencyMs: 1500,
                downloadMs: 4000,
                uploadMs: 0,
                bidirectionalMs: 0,
              },
              pingCadence: "medium",
              loadedPingCadence: "medium",
              adaptive: { enabled: false },
              transferStreams: { mode: "forced", count: 1 },
            },
          }),
        );
      },
      { servers, count },
    );
    console.log("GM_BENCH_STEP navigate");
    await page.goto(servers[0].url);
    console.log("GM_BENCH_STEP loaded");
    const settings = await openSettings(page);
    if (
      !(await settings
        .getByLabel("Save completed results on this device")
        .evaluate((input) => input.checked))
    )
      await settings
        .getByText("Save completed results on this device", { exact: true })
        .click();
    await expect(
      settings.locator('.readiness-badge[data-state="verified"]'),
    ).toBeVisible({ timeout: 15000 });
    await settings.getByRole("button", { name: "Close Settings" }).click();
    console.log("GM_BENCH_STEP ready");
    await page.evaluate(() => {
      const frames: number[] = [];
      let last = performance.now();
      const sample = (at: number) => {
        frames.push(at - last);
        last = at;
        (window as any).__gmFrameHandle = requestAnimationFrame(sample);
      };
      (window as any).__gmFrameGaps = frames;
      (window as any).__gmFrameHandle = requestAnimationFrame(sample);
    });
    console.log("GM_BENCH_BEGIN");
    await startTest(page);
    console.log("GM_BENCH_STEP started");
    await waitForCompletion(page, 30000);
    console.log("GM_BENCH_STEP completed");
    const saveDeadline = Date.now() + 10000;
    while (!(await page.evaluate(() => (window as any).__gmHistorySaved))) {
      if (Date.now() >= saveDeadline) {
        await page.artifact("server-collection-history-failure");
        throw new Error("Completed result was not saved within ten seconds");
      }
      await Bun.sleep(50);
    }
    const record = await page.evaluate(
      (schema) =>
        new Promise<HistoryRecord>((resolve, reject) => {
          const opening = indexedDB.open(schema.name, schema.version);
          opening.onerror = () => reject(opening.error);
          opening.onsuccess = () => {
            const db = opening.result;
            if (!db.objectStoreNames.contains(schema.resultsStore)) {
              db.close();
              reject(new Error("History was not initialized"));
              return;
            }
            const request = db
              .transaction(schema.resultsStore)
              .objectStore(schema.resultsStore)
              .getAll();
            request.onerror = () => {
              db.close();
              reject(request.error);
            };
            request.onsuccess = () => {
              db.close();
              resolve(
                request.result.sort(
                  (a: HistoryRecord, b: HistoryRecord) =>
                    b.completedAt - a.completedAt,
                )[0],
              );
            };
          };
        }),
      HISTORY_DB,
    );
    const frames = await page.evaluate(() => {
      cancelAnimationFrame((window as any).__gmFrameHandle);
      const frames: number[] = (window as any).__gmFrameGaps;
      frames.sort((a, b) => a - b);
      return {
        count: frames.length,
        p95Ms: frames[Math.ceil(frames.length * 0.95) - 1],
        maxMs: frames.at(-1),
      };
    });
    const details = record.multiServer!;
    if (
      details.failures.length ||
      details.participants.length !== count ||
      !record.stages.download.result
    )
      throw new Error(
        `Invalid measurement cell: ${JSON.stringify(details.failures)}`,
      );
    console.log(
      "GM_BENCH_END " +
        JSON.stringify({
          count,
          downloadMbps:
            (record.stages.download.result.reportedBytesPerSec * 8) / 1e6,
          receiverWindows: details.intervals.filter(
            (interval) => interval.stage === "download",
          ),
          latency: details.servers.map((server) => ({
            id: server.server.id,
            idle: server.latencyByStage.latency,
            loaded: server.latencyByStage.download,
          })),
          frames,
          durationMs: record.durationMs,
        }),
    );
    // Give the external process sampler time to capture the final live browser tree.
    await Bun.sleep(300);
  } finally {
    page.close();
    Bun.WebView.closeAll();
  }
}, 60000);
