import {
  baseConfig,
  closeSettings,
  frankfurt,
  home,
  open,
  openSettings,
  ready,
  run,
} from "./fleet";
import { expect, test } from "./webview";

const config = {
  ...baseConfig,
  transports: { throughputTarget: "protocol:http1", latencyTarget: "auto" },
};

test("repeated multi-server runs keep a working fallback without probing the failed preferred path again", async (page) => {
  await page.addInitScript((peer) => {
    const original = window.fetch.bind(window);
    Object.assign(window, { proofs: 0 });
    window.fetch = ((input: RequestInfo | URL, init?: RequestInit) => {
      const url = new URL(String(input), location.href);
      if (url.pathname === "/probe") {
        (window as any).proofs++;
        if (url.origin === peer) return Promise.reject(new TypeError("Preferred path unavailable"));
      }
      return original(input, init);
    }) as typeof fetch;
  }, frankfurt.h3);
  await open(page, home.url, { servers: [home, frankfurt], config });
  await ready(page);
  const proofs = await page.evaluate<number>(() => (window as any).proofs);
  for (let n = 0; n < 2; n++) {
    const saved = await run(page);
    expect(saved.result.outcome).toBe("complete");
    expect(await page.locator(".server-results").getAttribute("open")).toBeNull();
    expect(
      saved.result.multiServer.servers.find((s) => s.server.id === frankfurt.id)?.latencyTarget
        ?.transport,
    ).toBe("websocket");
    expect(await page.evaluate<number>(() => (window as any).proofs)).toBe(proofs);
  }
});

for (const all of [false, true])
  test(
    all
      ? "missing latency on every server skips the empty stage and preserves transfers"
      : "a failed latency path preserves both servers' download and upload and stays a latency failure",
    async (page) => {
      await page.addInitScript(
        ({ peers, latency }) => {
          const original = window.fetch.bind(window);
          window.fetch = (async (input: RequestInfo | URL, init?: RequestInit) => {
            const url = new URL(String(input), location.href);
            if (latency.includes(url.origin) && url.pathname === "/probe")
              throw new TypeError("Latency path unavailable");
            const response = await original(input, init);
            if (!peers.includes(url.origin) || url.pathname !== "/preflight") return response;
            const document = await response.json();
            document.capabilities.latency = document.capabilities.latency.filter(
              (target: { transport: string }) => target.transport === "webtransport",
            );
            return Response.json(document, { headers: response.headers });
          }) as typeof fetch;
        },
        {
          peers: all ? [home.url, frankfurt.url] : [frankfurt.url],
          latency: all ? [home.h3, frankfurt.h3] : [frankfurt.h3],
        },
      );
      await open(page, home.url, {
        servers: [home, frankfurt],
        config: {
          ...config,
          duration: { ...baseConfig.duration, latencyMs: all ? 20_000 : 1000 },
        },
      });
      const settings = await openSettings(page);
      await expect(settings.locator('.server-status[data-state="failed"]')).toHaveCount(
        all ? 2 : 1,
      );
      await closeSettings(page);
      const saved = await run(page);
      expect(saved.result.multiServer.participants).toEqual(["self", frankfurt.id]);
      const peer = saved.result.multiServer.servers.find((s) => s.server.id === frankfurt.id)!;
      expect(peer.latencyTarget).toBeNull();
      expect(peer.download?.reportedBytesPerSec).toBeGreaterThan(0);
      expect(peer.upload?.reportedBytesPerSec).toBeGreaterThan(0);
      expect(peer.stages.download).toBe("complete");
      expect(peer.stages.upload).toBe("complete");
      expect(saved.result.multiServer.failures.map((f) => f.serverId)).toEqual(
        all ? ["self", frankfurt.id] : [frankfurt.id],
      );
      if (all) expect(saved.result.durationMs).toBeLessThan(20_000);
      expect(saved.result.multiServer.failures.every((f) => f.scope === "latency")).toBe(true);
      await openSettings(page);
      await expect(
        settings
          .getByRole("group", { name: "Throughput path" })
          .locator('.validation .status-dot[data-tone="ok"]'),
      ).toHaveCount(1);
      expect(await page.locator(".server-results").getAttribute("open")).toBeNull();
      await page.artifact(`multiserver-${all ? "all-latency" : "latency"}-failure-collapsed`);
      await page.locator(".server-results > summary").click();
      const row = page.locator(".server-grid tbody tr", { hasText: "Frankfurt" });
      await expect(row.locator(".causes li")).toHaveCount(1);
      await expect(row.locator(".causes li")).toContainText(
        saved.result.multiServer.failures.find((f) => f.serverId === frankfurt.id)!.message,
      );
      await page.artifact(`multiserver-${all ? "all-latency" : "latency"}-failure`);
    },
  );
