import { home, open, ready, run, runButton, savedResult } from "../e2e/fleet";
import { expect, settled, test } from "../e2e/webview";

test("one page stays responsive through repeated runs and History visits", async (page) => {
  await page.addInitScript(() => {
    const pending = new Set<number>();
    const raf = window.requestAnimationFrame.bind(window);
    const cancel = window.cancelAnimationFrame.bind(window);
    const stats = { requests: 0, workers: 0, pending, tasks: [] as number[] };
    window.requestAnimationFrame = (fn) => {
      stats.requests++;
      const id = raf((now) => {
        pending.delete(id);
        fn(now);
      });
      pending.add(id);
      return id;
    };
    window.cancelAnimationFrame = (id) => {
      pending.delete(id);
      cancel(id);
    };
    const Original = window.Worker;
    window.Worker = class extends Original {
      constructor(...args: ConstructorParameters<typeof Worker>) {
        super(...args);
        stats.workers++;
      }
      terminate() {
        stats.workers--;
        super.terminate();
      }
    };
    new PerformanceObserver((list) => {
      stats.tasks.push(...list.getEntries().map((entry) => entry.duration));
    }).observe({ type: "longtask" });
    Object.assign(window, { sessionStats: stats });
  });
  await open(page, home.http, {
    config: {
      stages: {
        latency: true,
        download: true,
        upload: true,
        bidirectional: true,
      },
      transports: {
        throughputTarget: "protocol:http1",
        latencyTarget: "transport:websocket",
      },
    },
  });
  await ready(page);
  await page.cdp("Emulation.setCPUThrottlingRate", { rate: 4 });
  const samples = [];
  for (let i = 0; i < Number(process.env.GM_PERF_RUNS ?? 12); i++) {
    if (process.env.GM_PERF_BACKGROUND && (i + 1) % 10 === 0) {
      await page.evaluate(settled);
      const { windowId } = await page.cdp("Browser.getWindowForTarget");
      const bounds = (windowState: string) =>
        page.cdp("Browser.setWindowBounds", {
          windowId,
          bounds: { windowState },
        });
      const started = Date.now();
      await runButton(page, /^(Start test|Run again)$/).click();
      await bounds("minimized");
      expect(await page.evaluate(() => document.hidden)).toBe(true);
      const saved = await savedResult(page, started, 20_000);
      expect(saved.result.outcome).toBe("complete");
      await bounds("normal");
      await expect(page.locator('#console[data-phase="complete"]')).toHaveCount(
        1,
      );
    } else {
      const saved = await run(page);
      expect(saved.result.outcome).toBe("complete");
    }
    await page.goto(`${home.http}/#/history`);
    await expect(page.locator(".result-row")).toHaveCount(i + 1);
    await page.goto(`${home.http}/#/`);
    await Bun.sleep(1000);
    await page.cdp("HeapProfiler.collectGarbage");
    const counters = await page.cdp("Memory.getDOMCounters");
    const heap = await page.cdp("Runtime.getHeapUsage");
    const stats = await page.evaluate(() => {
      const stats = (window as any).sessionStats;
      return {
        frames: stats.requests,
        pending: stats.pending.size,
        workers: stats.workers,
        maxAnchors: Math.max(
          0,
          ...Array.from(
            document.querySelectorAll<HTMLElement>("[style]"),
            (node) =>
              node.style
                .getPropertyValue("anchor-name")
                .split(",")
                .filter((name) => name.trim().startsWith("--gm-tt-")).length,
          ),
        ),
        longestTaskMs: Math.max(0, ...stats.tasks.splice(0)),
      };
    });
    samples.push({ run: i + 1, ...stats, ...counters, heap: heap.usedSize });
    console.log(samples.at(-1));
  }
  console.table(samples);
  if (process.env.GM_PERF_METRICS)
    await Bun.write(process.env.GM_PERF_METRICS, JSON.stringify(samples));
  if (process.env.GM_PERF_HEAP) {
    const chunks: string[] = [];
    page.onCdp("HeapProfiler.addHeapSnapshotChunk", (event: any) =>
      chunks.push(event.data.chunk),
    );
    await page.cdp("HeapProfiler.takeHeapSnapshot");
    await Bun.write(process.env.GM_PERF_HEAP, chunks.join(""));
  }
  expect(samples.at(-1)!.workers).toBe(samples[0].workers);
  expect(samples.at(-1)!.nodes - samples[0].nodes).toBeLessThan(100);
  expect(
    samples.at(-1)!.jsEventListeners - samples[0].jsEventListeners,
  ).toBeLessThan(10);
  expect(samples.at(-1)!.pending).toBe(0);
  for (const sample of samples)
    expect(sample.maxAnchors).toBeLessThanOrEqual(1);
});
