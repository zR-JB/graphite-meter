import {
  amsterdam,
  closeSettings,
  frankfurt,
  helsinki,
  home,
  open,
  openSettings,
  phase,
  ready,
  run,
  runButton,
  savedResult,
} from "./fleet";
import type { Server } from "./servers";
import { expect, test, type Page } from "./webview";

interface Network {
  failed: string[];
  hanging: string[];
  catalogues: string[];
  preflights: Record<string, number>;
}
const origins = (...servers: Server[]) =>
  servers.flatMap((server) => [server.http, server.url, server.h2, server.h3]);

// Only browser reachability is simulated; measurement traffic stays real.
async function limit(page: Page, failed: Server[], hanging: Server[]) {
  await page.addInitScript(
    (network: Network) => {
      Object.assign(window, { network });
      const original = window.fetch.bind(window);
      window.fetch = ((input: RequestInfo | URL, init?: RequestInit) => {
        const url = new URL(
          input instanceof Request ? input.url : String(input),
          location.href,
        );
        if (url.pathname === "/servers") network.catalogues.push(url.origin);
        if (url.pathname === "/preflight")
          network.preflights[url.origin] =
            (network.preflights[url.origin] ?? 0) + 1;
        if (network.failed.includes(url.origin))
          return Promise.reject(new TypeError("Failed to fetch"));
        if (!network.hanging.includes(url.origin)) return original(input, init);
        return new Promise((_resolve, reject) => {
          const signal = init?.signal;
          signal?.addEventListener("abort", () => reject(signal.reason));
        });
      }) as typeof fetch;
    },
    {
      failed: origins(...failed),
      hanging: origins(...hanging),
      catalogues: [],
      preflights: {},
    },
  );
}
const network = (page: Page) =>
  page.evaluate<Network>(() => (window as any).network);
const selection = (page: Page) =>
  page.evaluate(() =>
    JSON.parse(localStorage.getItem("graphite-meter:server-selection:v1")!).map(
      (server: { id: string }) => server.id,
    ),
  );
const http1 = {
  transports: {
    throughputTarget: "protocol:http1",
    latencyTarget: "transport:websocket",
  },
};

test("failed and hanging unselected peers do not hold a selected pair", async (page) => {
  await limit(page, [amsterdam], [helsinki]);
  await open(page, home.url, { servers: [home, frankfurt], config: http1 });
  await ready(page);
  const saved = await run(page);
  expect(saved.outcome).toBe("complete");
  expect(saved.multiServer?.participants).toEqual(["self", "server-1"]);

  const settings = await openSettings(page);
  const choices = settings.getByRole("group", { name: "Servers to test" });
  await expect(choices.getByRole("checkbox")).toHaveCount(5);
  await expect(choices).toHaveAttribute("aria-busy", "true");
  await expect(choices).toHaveAttribute("aria-busy", "false", {
    timeout: 10_000,
  });
  const settled = (await network(page)).preflights;
  await Bun.sleep(1000);
  expect((await network(page)).preflights).toEqual(settled);
  expect((await network(page)).catalogues).toEqual([home.url]);
  expect(await selection(page)).toEqual(["self", "server-1"]);
});

test("a saved unreachable peer stays selected through cancel and retry", async (page) => {
  await limit(page, [], [frankfurt]);
  await open(page, home.url, { servers: [home, frankfurt], config: http1 });
  await runButton(page, "Start test").click();
  await runButton(page, "Cancel").click();
  await expect(runButton(page, "Start test")).toBeVisible();
  await expect(page.locator(".gauge-status.error")).toHaveCount(0);
  expect(await selection(page)).toEqual(["self", "server-1"]);

  await page.evaluate(() => {
    const state = (window as any).network as Network;
    state.failed.push(...state.hanging.splice(0));
  });
  await runButton(page, "Start test").click();
  await expect(page.locator(".gauge-status.error")).toBeVisible();
  await expect(page.locator(".gauge-notes")).toContainText("Frankfurt");
  const settings = await openSettings(page);
  const retry = settings.getByRole("button", { name: "Retry Frankfurt" });
  await expect(settings.locator(".server-feedback")).toContainText(
    /timed out|could not be reached/,
  );
  await expect(settings.getByRole("button", { name: /Sign in/ })).toHaveCount(
    0,
  );
  const before = (await network(page)).preflights[frankfurt.url];
  await retry.click();
  await expect
    .poll(async () => (await network(page)).preflights[frankfurt.url])
    .toBeGreaterThan(before);
  expect(await selection(page)).toEqual(["self", "server-1"]);
  await settings.getByRole("checkbox", { name: /^Frankfurt/ }).click();
  await closeSettings(page);
  await ready(page);
  expect(await selection(page)).toEqual(["self"]);
  expect((await run(page)).multiServer?.participants).toEqual(["self"]);
});

test("a result finished after the network is blocked is still saved", async (page) => {
  await open(page, home.url, {
    config: {
      stages: { latency: false, download: true, upload: false },
      skipLoadedLatencyWhenStageOff: true,
    },
  });
  await ready(page);
  const startedAt = Date.now();
  await runButton(page, "Start test").click();
  await expect(phase(page, "download")).toHaveCount(1);
  await page.blockRequests(["*"]);
  const saved = await savedResult(page, startedAt);
  expect(saved.stages.download.result?.reportedBytesPerSec).toBeGreaterThan(0);
});

test("a blocked latency probe fails only its path and Retry stays scoped", async (page) => {
  const probe = `${home.url}/probe*`;
  await page.blockRequests([probe]);
  await open(page, home.url, {
    config: {
      transports: {
        throughputTarget: "protocol:http2",
        latencyTarget: "transport:websocket",
      },
    },
  });
  const settings = await openSettings(page);
  const retryLatency = settings.getByRole("button", {
    name: "Retry Latency path",
  });
  await expect(retryLatency).toBeVisible({ timeout: 10_000 });
  await expect(
    settings.getByRole("button", { name: "Retry Throughput path" }),
  ).toHaveCount(0);
  await expect(settings.locator('[data-readiness="failed"]')).toBeVisible();
  await page.blockRequests([]);
  await retryLatency.click();
  await expect(settings.locator('[data-readiness="verified"]')).toBeVisible({
    timeout: 10_000,
  });
});
