// Drives the shipped UI and coordinator through the isolated Linux network rig.
import { test } from "bun:test";
import { ready, run } from "../e2e/fleet";
import { Page } from "../e2e/webview";
import type { ThroughputResult } from "../src/lib/runner/contract";
import { DEFAULT_CONFIG } from "../src/lib/state/defaults";

const servers: { id: string; url: string; name: string }[] = JSON.parse(
  process.env.GM_MULTI_BENCH_SERVERS!,
);
const count = Number(process.env.GM_MULTI_BENCH_COUNT);
if (![1, 2, 4].includes(count))
  throw new Error("Select a 1-, 2-, or 4-server cell");
// Matrix cells measure the product defaults with the rig's overrides.
const matrix = process.env.GM_MULTI_BENCH_CONFIG;
const config = matrix
  ? { ...DEFAULT_CONFIG, ...JSON.parse(matrix) }
  : {
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
    };
const TRANSFERS = ["download", "upload", "bidirectional"] as const;
const mbps = (result: ThroughputResult | null | undefined) =>
  result ? (result.reportedBytesPerSec * 8) / 1e6 : null;

test("coordinated server collection cell", async () => {
  const page = new Page();
  try {
    console.log("GM_BENCH_STEP init");
    await page.addInitScript(
      ({ servers, count, config }) => {
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
          JSON.stringify({ resultHistoryPreference: "enabled", config }),
        );
      },
      { servers, count, config },
    );
    console.log("GM_BENCH_STEP navigate");
    await page.goto(servers[0].url);
    console.log("GM_BENCH_STEP loaded");
    await ready(page);
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
    const record = await run(page, 60_000);
    console.log("GM_BENCH_STEP completed");
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
    const result = record.result;
    const details = result.multiServer;
    if (
      details.failures.length ||
      details.participants.length !== count ||
      TRANSFERS.some(
        (stage) => config.stages[stage] && result.stages[stage] !== "complete",
      )
    )
      throw new Error(
        `Invalid measurement cell: ${JSON.stringify(details.failures)}`,
      );
    const stage = (
      name: (typeof TRANSFERS)[number],
      down: ThroughputResult | null | undefined,
      up: ThroughputResult | null | undefined,
    ) => {
      const latency = result.latencyByStage[name];
      return {
        outcome:
          result.stages[name] === "complete" ? "Complete" : result.stages[name],
        downMbps: mbps(down),
        upMbps: mbps(up),
        latencyP50Ms: latency?.p50Ms ?? null,
        latencyP95Ms: latency?.p95Ms ?? null,
        replies: latency ? latency.probeCount - latency.timeoutCount : null,
        timeouts: latency?.timeoutCount ?? null,
      };
    };
    console.log(
      "GM_BENCH_END " +
        JSON.stringify({
          count,
          downloadMbps: mbps(result.download),
          receiverWindows: details.intervals.filter(
            (interval) => interval.stage === "download",
          ),
          latency: details.servers.map((server) => ({
            id: server.server.id,
            idle: server.latencyByStage.latency,
            loaded: server.latencyByStage.download,
          })),
          frames,
          durationMs: result.durationMs,
          browser: (await page.cdp("Browser.getVersion")).product,
          paths: details.servers.map((server) => server.throughput),
          stages: {
            download: stage("download", result.download, null),
            upload: stage("upload", null, result.upload),
            bidirectional: stage(
              "bidirectional",
              result.bidirectional?.down,
              result.bidirectional?.up,
            ),
          },
        }),
    );
    // Give the external process sampler time to capture the final live browser tree.
    await Bun.sleep(300);
  } finally {
    page.close();
    Bun.WebView.closeAll();
  }
}, 120_000);
