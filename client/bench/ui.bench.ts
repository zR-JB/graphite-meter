import { DEFAULT_CONFIG } from "../src/lib/state/defaults";
import { fleet, home, open, ready, run } from "../e2e/fleet";
import { expect, test } from "../e2e/webview";

interface Frames {
  frames: number[];
  tasks: number[];
}

const counts = process.env.GM_PERF_SERVERS?.split(",").map(Number) ?? [1, 4];
const bidirectional = process.env.GM_PERF_BIDIRECTIONAL === "1";
const cpu = Number(process.env.GM_PERF_CPU ?? 1);
const viewport = process.env.GM_PERF_VIEWPORT?.split("x").map(Number);
// A 60 Hz cadence, with 1 ms tolerance for timestamp rounding and scheduling noise.
const target = Number(process.env.GM_PERF_TARGET_MS ?? 16.7);
const tail = Number(process.env.GM_PERF_P99_MS ?? (cpu > 1 ? 33.4 : 16.7));
const stall = Number(process.env.GM_PERF_MAX_MS ?? 50);
for (const count of counts)
  test(
    `${count} servers keep frames and default adaptive results`,
    async (page) => {
      if (viewport)
        await page.cdp("Emulation.setDeviceMetricsOverride", {
          width: viewport[0],
          height: viewport[1],
          deviceScaleFactor: Number(process.env.GM_PERF_DPR ?? 1),
          mobile: viewport[0] <= 600,
        });
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
          state().tasks.push(
            ...list.getEntries().map((entry) => entry.duration),
          ),
        ).observe({ type: "longtask", buffered: true });
      });
      const servers = [
        { id: "self", url: home.http },
        ...fleet.slice(1, count),
      ];
      await open(page, home.http, {
        servers,
        config: {
          ...DEFAULT_CONFIG,
          stages: { ...DEFAULT_CONFIG.stages, bidirectional },
          transports: {
            throughputTarget: "protocol:http1",
            latencyTarget: "transport:websocket",
          },
        },
      });
      await ready(page);
      if (process.env.GM_PERF_CPU)
        await page.cdp("Emulation.setCPUThrottlingRate", {
          rate: Number(process.env.GM_PERF_CPU),
        });
      if (process.env.GM_PERF_PROFILE) {
        await page.cdp("Profiler.enable");
        await page.cdp("Profiler.start");
      }
      await page.cdp("Performance.enable");
      const before = await page.cdp("Performance.getMetrics");
      const saved = await run(page, 60_000);
      const after = await page.cdp("Performance.getMetrics");
      const rendering = after.metrics
        .filter(({ name }: { name: string }) =>
          [
            "LayoutDuration",
            "RecalcStyleDuration",
            "ScriptDuration",
            "TaskDuration",
          ].includes(name),
        )
        .map(({ name, value }: { name: string; value: number }) => ({
          name,
          ms:
            (value -
              before.metrics.find(
                (metric: { name: string }) => metric.name === name,
              ).value) *
            1000,
        }));
      console.table(rendering);
      if (bidirectional) {
        expect(
          saved.result.bidirectional?.down?.reportedBytesPerSec,
        ).toBeGreaterThan(0);
        expect(
          saved.result.bidirectional?.up?.reportedBytesPerSec,
        ).toBeGreaterThan(0);
      }
      if (process.env.GM_PERF_PROFILE) {
        const { profile } = await page.cdp("Profiler.stop");
        await Bun.write(
          `${process.env.GM_PERF_PROFILE}-${count}.json`,
          JSON.stringify(profile),
        );
      }
      for (const stage of ["download", "upload"] as const)
        expect(saved.result[stage]?.reportedBytesPerSec).toBeGreaterThan(0);
      const metrics = await page.evaluate(() =>
        Object.entries((window as any).phases as Record<string, Frames>).map(
          ([phase, { frames, tasks }]) => {
            const sorted = frames.toSorted((a, b) => a - b);
            return {
              phase,
              frames: sorted.length,
              p60Ms: sorted[Math.floor(sorted.length * 0.6)] ?? 0,
              p70Ms: sorted[Math.floor(sorted.length * 0.7)] ?? 0,
              p95Ms: sorted[Math.floor(sorted.length * 0.95)] ?? 0,
              p99Ms: sorted[Math.floor(sorted.length * 0.99)] ?? 0,
              maxMs: sorted.at(-1) ?? 0,
              longestTaskMs: Math.max(0, ...tasks),
            };
          },
        ),
      );
      console.table(metrics);
      if (process.env.GM_PERF_METRICS)
        await Bun.write(
          `${process.env.GM_PERF_METRICS}-${count}.json`,
          JSON.stringify({
            cpu,
            viewport,
            budget: { target, tail, stall },
            metrics,
            rendering,
          }),
        );
      for (const phase of metrics.filter((entry) => entry.frames > 30)) {
        expect(phase.p95Ms).toBeLessThan(100);
        expect(phase.longestTaskMs).toBeLessThan(500);
        if (
          ["latency", "download", "upload", "bidirectional"].includes(
            phase.phase,
          )
        ) {
          expect(phase.p95Ms).toBeLessThanOrEqual(target + 1);
          expect(phase.p99Ms).toBeLessThanOrEqual(tail + 1);
          expect(phase.maxMs).toBeLessThanOrEqual(stall + 1);
          expect(phase.longestTaskMs).toBeLessThan(stall);
        }
      }
    },
    { monitorDisplay: false },
  );
