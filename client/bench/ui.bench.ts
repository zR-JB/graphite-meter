import { DEFAULT_CONFIG } from "../src/app/preferences";
import { fleet, home, open, ready, run } from "../e2e/fleet";
import { expect, test } from "../e2e/webview";

/** What the page recorded, all on its own clock: each phase as it began, each frame, each long task. */
interface Recording {
  timeline: [number, string][];
  frames: number[];
  tasks: [number, number][];
}

const counts = process.env.GM_PERF_SERVERS?.split(",").map(Number) ?? [1, 4];
const bidirectional = process.env.GM_PERF_BIDIRECTIONAL === "1";
const cpu = Number(process.env.GM_PERF_CPU ?? 1);
const viewport = process.env.GM_PERF_VIEWPORT?.split("x").map(Number) as [number, number] | undefined;
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
      // Phases are timed as they change, not read when an observer reports, so a task or a frame lands in the
      // phase it ran in.
      await page.addInitScript(() => {
        const recording: Recording = ((window as any).recording = { timeline: [], frames: [], tasks: [] });
        let phase = "";
        const note = () => {
          const now = document.querySelector("#console")?.getAttribute("data-phase") ?? "startup";
          if (now === phase) return;
          phase = now;
          recording.timeline.push([performance.now(), now]);
        };
        note();
        new MutationObserver(note).observe(document, { subtree: true, attributeFilter: ["data-phase"] });
        // The console arrives with its phase set; watching insertions stops once it is in.
        const arrival = new MutationObserver(() => {
          if (!document.getElementById("console")) return;
          arrival.disconnect();
          note();
        });
        arrival.observe(document, { subtree: true, childList: true });
        const frame = (now: number) => {
          recording.frames.push(now);
          requestAnimationFrame(frame);
        };
        requestAnimationFrame(frame);
        new PerformanceObserver((list) =>
          recording.tasks.push(
            ...list.getEntries().map((entry): [number, number] => [entry.startTime, entry.duration]),
          ),
        ).observe({ type: "longtask", buffered: true });
      });
      const servers = [{ id: "self", url: home.http }, ...fleet.slice(1, count)];
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
          ["LayoutDuration", "RecalcStyleDuration", "ScriptDuration", "TaskDuration"].includes(name),
        )
        .map(({ name, value }: { name: string; value: number }) => ({
          name,
          ms: (value - before.metrics.find((metric: { name: string }) => metric.name === name).value) * 1000,
        }));
      console.table(rendering);
      if (bidirectional) {
        expect(saved.result.bidirectional?.down?.reportedBytesPerSec).toBeGreaterThan(0);
        expect(saved.result.bidirectional?.up?.reportedBytesPerSec).toBeGreaterThan(0);
      }
      if (process.env.GM_PERF_PROFILE) {
        const { profile } = await page.cdp("Profiler.stop");
        await Bun.write(`${process.env.GM_PERF_PROFILE}-${count}.json`, JSON.stringify(profile));
      }
      // Only a run every selected server measured in full is this cell's workload.
      expect(saved.result.outcome).toBe("complete");
      expect(saved.result.multiServer.failures).toEqual([]);
      const stages = bidirectional
        ? (["download", "upload", "bidirectional"] as const)
        : (["download", "upload"] as const);
      expect(saved.result.multiServer.servers).toHaveLength(count);
      for (const stage of stages) {
        expect(saved.result.stages[stage]).toBe("complete");
        for (const server of saved.result.multiServer.servers)
          if (stage === "bidirectional") expect(server.bidirectional?.down?.reportedBytesPerSec).toBeGreaterThan(0);
          else expect(server[stage]?.reportedBytesPerSec).toBeGreaterThan(0);
      }
      const measuredMs = Object.fromEntries(
        stages.map((stage) => {
          const spans = saved.result.multiServer.intervals.filter((interval) => interval.stage === stage);
          return [stage, spans.length ? Math.round(spans.at(-1)!.endMs - spans[0]!.startMs) : 0];
        }),
      );
      const metrics = await page.evaluate(() => {
        const { timeline, frames, tasks } = (window as any).recording as Recording;
        const phaseAt = (at: number) => timeline.findLast(([since]) => since <= at)?.[1] ?? "startup";
        const by = new Map<string, { frames: number[]; tasks: number[] }>();
        const slot = (phase: string) => by.get(phase) ?? by.set(phase, { frames: [], tasks: [] }).get(phase)!;
        // A frame counts in the phase it began in, and also in the one it ended in when it spans a change.
        for (let i = 1; i < frames.length; i++) {
          const [from, to] = [phaseAt(frames[i - 1]!), phaseAt(frames[i]!)];
          slot(from).frames.push(frames[i]! - frames[i - 1]!);
          if (to !== from) slot(to).frames.push(frames[i]! - frames[i - 1]!);
        }
        // A task counts in every phase it overlapped.
        for (const [start, duration] of tasks)
          for (const phase of new Set(
            timeline
              .filter(([since], i) => since < start + duration && (timeline[i + 1]?.[0] ?? Infinity) > start)
              .map(([, p]) => p),
          ))
            slot(phase).tasks.push(duration);
        const rows = [...by].map(([phase, { frames: gaps, tasks: long }]) => {
          const sorted = gaps.toSorted((a, b) => a - b);
          return {
            phase,
            frames: sorted.length,
            p60Ms: sorted[Math.floor(sorted.length * 0.6)] ?? 0,
            p70Ms: sorted[Math.floor(sorted.length * 0.7)] ?? 0,
            p95Ms: sorted[Math.floor(sorted.length * 0.95)] ?? 0,
            p99Ms: sorted[Math.floor(sorted.length * 0.99)] ?? 0,
            maxMs: sorted.at(-1) ?? 0,
            longestTaskMs: Math.max(0, ...long),
          };
        });
        const gaps = frames.slice(1).map((at, i) => at - frames[i]!);
        return {
          rows,
          worstFrameMs: Math.max(0, ...gaps),
          longestTaskMs: Math.max(0, ...tasks.map(([, duration]) => duration)),
        };
      });
      console.table(metrics.rows);
      console.log(
        `whole run: worst frame ${metrics.worstFrameMs.toFixed(1)} ms, longest task ${metrics.longestTaskMs} ms`,
      );
      if (process.env.GM_PERF_METRICS)
        await Bun.write(
          `${process.env.GM_PERF_METRICS}-${count}.json`,
          JSON.stringify({
            cpu,
            viewport,
            budget: { target, tail, stall },
            measuredMs,
            browser: (await page.cdp("Browser.getVersion")).product,
            metrics,
            rendering,
          }),
        );
      expect(metrics.longestTaskMs).toBeLessThan(500);
      for (const phase of metrics.rows.filter((entry) => entry.frames > 30)) {
        expect(phase.p95Ms).toBeLessThan(100);
        expect(phase.longestTaskMs).toBeLessThan(500);
        if (["latency", "download", "upload", "bidirectional"].includes(phase.phase)) {
          expect(phase.p95Ms).toBeLessThanOrEqual(target + 1);
          expect(phase.p99Ms).toBeLessThanOrEqual(tail + 1);
          expect(phase.maxMs).toBeLessThanOrEqual(stall + 1);
          expect(phase.longestTaskMs).toBeLessThan(stall);
        }
      }
    },
    { monitorDisplay: false },
  );
