import { test, expect } from "./multi-server-fixtures";
import { configure, ready } from "./multi-server-actions";
import { startTest, waitForCompletion, openSettings } from "../browser/webview";

type PhaseFrames = { frames: number[]; tasks: number[] };
declare global {
  interface Window {
    clientFrames: Record<string, PhaseFrames>;
  }
}

for (const count of [1, 4])
  test(`${count} real servers keep the latency stage responsive across dense reply batches`, async ({
    page,
  }) => {
    await page.addInitScript(() => {
      window.clientFrames = {};
      let last = performance.now();
      const phase = () =>
        document.querySelector<HTMLElement>(".gauge-panel")?.dataset.phase ??
        "startup";
      const state = () =>
        (window.clientFrames[phase()] ??= { frames: [], tasks: [] });
      const frame = (now: number) => {
        state().frames.push(now - last);
        last = now;
        requestAnimationFrame(frame);
      };
      requestAnimationFrame(frame);
      new PerformanceObserver((list) =>
        state().tasks.push(...list.getEntries().map((entry) => entry.duration)),
      ).observe({ type: "longtask", buffered: true });
    });
    await configure(
      page,
      ["self", "server-1", "server-2", "server-3"].slice(0, count),
      1000,
      {
        duration: {
          warmupMs: 250,
          latencyMs: 10000,
          downloadMs: 1200,
          uploadMs: 1200,
          bidirectionalMs: 1200,
        },
        transports: {
          throughputTarget: "protocol:http1",
          latencyTarget: "transport:websocket",
        },
      },
    );
    await ready(page);
    await startTest(page);
    await expect
      .poll(() => page.locator(".gauge-panel").getAttribute("data-phase"))
      .toBe("latency");
    await page.waitForTimeout(4000);
    const settings = await openSettings(page);
    await expect(
      settings.getByRole("button", { name: "Close Settings" }),
    ).toBeVisible();
    await settings.getByRole("button", { name: "Close Settings" }).click();
    await waitForCompletion(page, 30000);
    const metrics = await page.evaluate(() =>
      Object.fromEntries(
        Object.entries(window.clientFrames).map(([phase, data]) => {
          const sorted = data.frames.toSorted((a, b) => a - b);
          return [
            phase,
            {
              frames: sorted.length,
              p95Ms: sorted[Math.floor(sorted.length * 0.95)] ?? 0,
              longestTaskMs: Math.max(0, ...data.tasks),
              longestFrameMs: Math.max(0, ...sorted),
            },
          ];
        }),
      ),
    );
    console.info("Real-client frame delivery", count, metrics);
    // Reject the seconds-long starvation regression with room for shared CI machines and renderer startup.
    expect(metrics.latency.frames).toBeGreaterThan(100);
    for (const stage of ["latency", "download", "upload", "bidirectional"]) {
      expect(metrics[stage].p95Ms).toBeLessThan(100);
      expect(metrics[stage].longestTaskMs).toBeLessThan(500);
    }
    await page.artifact(`client-performance-${count}-servers`);
  });
