import { test, expect } from "./multi-server-fixtures";
import { configure, ready, savedResult } from "./multi-server-actions";
import { startTest, waitForCompletion, openSettings } from "../browser/webview";
import { DEFAULT_CONFIG } from "../src/lib/state/defaults";

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
      let last: number | null = null;
      const phase = () =>
        document.querySelector<HTMLElement>(".gauge-panel")?.dataset.phase ??
        "startup";
      const state = () =>
        (window.clientFrames[phase()] ??= { frames: [], tasks: [] });
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

// Clear HTTP uses the ordinary browser clock resolution that exposed a same-tick
// final progress flush erasing an otherwise complete adaptive download window.
for (const count of [1, 2])
  test(`${count} clear HTTP servers retain default adaptive results with browser inspection active`, async ({
    page,
    context,
  }) => {
    const base = Number(process.env.GM_E2E_PORT_BASE ?? 7256) + 128;
    const servers = ["self", "peer"].map((id, index) => ({
      id,
      name: `Clear ${index + 1}`,
      url: `http://127.0.0.1:${base + index}`,
    }));
    const environment = Object.fromEntries(
      Object.entries(process.env).filter(
        ([key]) =>
          !key.startsWith("GM_AUTH_") && !key.startsWith("GM_SERVER_CATALOG"),
      ),
    );
    const processes = servers.map((server, index) =>
      Bun.spawn([process.env.GM_E2E_SERVER_BIN!], {
        env: {
          ...environment,
          GM_AUTH_MODE: "off",
          GM_H1_ADDR: new URL(server.url).host,
          GM_H1_TLS_ADDR: "",
          GM_H2_ADDR: "",
          GM_H3_ADDR: "",
          GM_TLS_CERT: "",
          GM_TLS_KEY: "",
          GM_SERVER_NAME: server.name,
          GM_SERVER_LOCATION: "Loopback HTTP fixture",
          ...(index === 0
            ? { GM_SERVER_CATALOG: JSON.stringify({ servers: [servers[1]] }) }
            : {}),
        },
        stdout: "ignore",
        stderr: Bun.file(`/tmp/graphite-meter-clear-${base}-${index}.log`),
      }),
    );
    try {
      for (const server of servers)
        await expect
          .poll(async () => {
            try {
              return (
                await fetch(`${server.url}/preflight`, {
                  signal: AbortSignal.timeout(500),
                })
              ).ok;
            } catch {
              return false;
            }
          })
          .toBe(true);
      await configure(page, ["self"], 10000, DEFAULT_CONFIG);
      await page.addInitScript(
        ({ servers, count }) => {
          localStorage.setItem(
            "graphite-meter:server-selection:v1",
            JSON.stringify(servers.slice(0, count)),
          );
          const prefs = JSON.parse(localStorage.getItem("graphite-meter:v1")!);
          prefs.latencySelection = { mode: "primary", serverId: "self" };
          localStorage.setItem("graphite-meter:v1", JSON.stringify(prefs));
          window.clientFrames = {};
          let last: number | null = null;
          const state = () => {
            const phase =
              document.querySelector<HTMLElement>(".gauge-panel")?.dataset
                .phase ?? "startup";
            return (window.clientFrames[phase] ??= { frames: [], tasks: [] });
          };
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
        },
        { servers, count },
      );
      await page.goto(servers[0].url);
      await ready(page);
      const inspector = await context.newCDPSession(page);
      for (const domain of ["Network", "DOM", "CSS", "Overlay", "Performance"])
        await inspector.send(`${domain}.enable`);
      await inspector.send("DOM.getDocument", { depth: -1, pierce: true });
      const started = Date.now();
      await startTest(page);
      await expect
        .poll(() => page.locator(".gauge-panel").getAttribute("data-phase"), {
          timeout: 30000,
        })
        .toBe("upload");
      await expect(
        page.getByRole("switch", {
          name: "Download stage",
        }),
      ).toHaveClass(/seg--complete/);
      await expect(
        page.locator(".result-chip", { hasText: "Download" }).locator(".num"),
      ).not.toHaveText("—");
      await page.setViewportSize({ width: 960, height: 720 });
      const settings = await openSettings(page);
      await expect(
        settings.getByRole("button", { name: "Close Settings" }),
      ).toBeVisible();
      await settings.getByRole("button", { name: "Close Settings" }).click();
      await page.setViewportSize({ width: 1920, height: 1080 });
      await waitForCompletion(page, 40000);
      await expect(
        page.locator(".result-card").getByText("Download", { exact: true }),
      ).toBeVisible();
      const saved = await savedResult(page, started);
      for (const stage of ["download", "upload"] as const) {
        expect(saved.stages[stage].status).toBe("complete");
        expect(saved.stages[stage].result?.reportedBytesPerSec).toBeGreaterThan(
          0,
        );
      }
      expect(saved.multiServer?.selection).toHaveLength(count);
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
      console.info("Clear HTTP inspector frame delivery", count, metrics);
      for (const stage of ["latency", "download", "upload"]) {
        expect(metrics[stage].frames).toBeGreaterThan(30);
        expect(metrics[stage].p95Ms).toBeLessThan(100);
        expect(metrics[stage].longestTaskMs).toBeLessThan(500);
      }
      await page.artifact(`clear-http-inspector-${count}-servers`);
    } finally {
      processes.forEach((process) => process.kill());
      await Promise.all(processes.map((process) => process.exited));
    }
  });
