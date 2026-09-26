import { DEFAULT_CONFIG } from "../src/lib/state/defaults";
import { fleet, home, open, ready, run } from "../e2e/fleet";
import { expect, test } from "../e2e/webview";

interface Frames {
  frames: number[];
  tasks: number[];
}

for (const count of [1, 4])
  test(`${count} servers keep frames and default adaptive results`, async (page) => {
    await page.addInitScript(() => {
      const phases: Record<string, Frames> = ((window as any).phases = {});
      const state = () =>
        (phases[
          document.querySelector("#console")?.getAttribute("data-phase") ??
            "startup"
        ] ??= { frames: [], tasks: [] });
      let last: number | null = null;
      const frame = (now: number) => {
        if (last !== null) state().frames.push(now - last);
        last = now;
        requestAnimationFrame(frame);
      };
      requestAnimationFrame(frame);
      new PerformanceObserver((list) =>
        state().tasks.push(...list.getEntries().map((entry) => entry.duration)),
      ).observe({ type: "longtask", buffered: true });
    });
    const servers = [{ id: "self", url: home.http }, ...fleet.slice(1, count)];
    await open(page, home.http, {
      servers,
      config: {
        ...DEFAULT_CONFIG,
        transports: {
          throughputTarget: "protocol:http1",
          latencyTarget: "transport:websocket",
        },
      },
    });
    await ready(page);
    const saved = await run(page, 60_000);
    for (const stage of ["download", "upload"] as const)
      expect(saved.stages[stage].result?.reportedBytesPerSec).toBeGreaterThan(
        0,
      );
    const metrics = await page.evaluate(() =>
      Object.entries((window as any).phases as Record<string, Frames>).map(
        ([phase, { frames, tasks }]) => {
          const sorted = frames.toSorted((a, b) => a - b);
          return {
            phase,
            frames: sorted.length,
            p95Ms: sorted[Math.floor(sorted.length * 0.95)] ?? 0,
            longestTaskMs: Math.max(0, ...tasks),
          };
        },
      ),
    );
    console.table(metrics);
    for (const phase of metrics.filter((entry) => entry.frames > 30)) {
      expect(phase.p95Ms).toBeLessThan(100);
      expect(phase.longestTaskMs).toBeLessThan(500);
    }
  });
