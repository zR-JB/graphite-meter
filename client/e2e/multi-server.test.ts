import { isHistoryRecord } from "../src/lib/history/types";
import {
  amsterdam,
  baseConfig,
  closeSettings,
  frankfurt,
  helsinki,
  home,
  open,
  openSettings,
  ready,
  run,
  savedResult,
} from "./fleet";
import { expect, test } from "./webview";

export const combined = {
  ...baseConfig,
  duration: { ...baseConfig.duration, downloadMs: 1000, uploadMs: 1000 },
};

test("four servers share one run and keep separate receiver windows", async (page) => {
  const four = [home, frankfurt, amsterdam, helsinki];
  await open(page, home.url, {
    servers: four,
    latency: { mode: "all", serverId: "self" },
    config: {
      ...combined,
      stages: { ...baseConfig.stages, bidirectional: true },
    },
  });
  await ready(page);
  const saved = await run(page);
  expect(isHistoryRecord(saved)).toBe(true);
  expect(saved.outcome).toBe("complete");
  expect(saved.multiServer?.failures).toEqual([]);
  expect(saved.multiServer?.participants).toEqual(four.map((s) => s.id));
  for (const stage of ["download", "upload", "bidirectional"] as const) {
    const interval = saved.multiServer!.intervals.find(
      (candidate) => candidate.stage === stage,
    )!;
    expect(interval.complete).toBe(true);
    expect(interval.participants).toHaveLength(4);
    const dirs = stage === "download" ? ["down"] : ["up"];
    for (const dir of stage === "bidirectional" ? ["down", "up"] : dirs)
      expect(interval.headline?.[dir as "down"]).toHaveLength(4);
  }
  for (const server of saved.multiServer!.servers) {
    expect(server.latencyByStage.latency?.probeCount).toBeGreaterThan(0);
    expect(server.totalBytes.down).toBeGreaterThan(0);
    expect(server.totalBytes.up).toBeGreaterThan(0);
  }

  const scope = page.getByRole("combobox", { name: "Result measurements" });
  await scope.fill("server-1");
  await expect(scope).toHaveValue("server-1");
  expect(await savedResult(page)).toEqual(saved);

  await page.evaluate((id) => (location.hash = `/history/${id}`), saved.id);
  await page.reload();
  await page.getByRole("button", { name: "Servers & paths" }).click();
  const servers = page.locator(".result-detail tbody tr");
  await expect(servers).toHaveCount(4);
  await expect(servers.nth(1)).toContainText(new URL(frankfurt.url).host);
  expect(await savedResult(page)).toEqual(saved);
});

test("an HTTP page without WebTransport verifies clear and TLS HTTP/1.1", async (page) => {
  await page.addInitScript(() =>
    Object.defineProperty(window, "WebTransport", { value: undefined }),
  );
  await open(page, home.http, {
    servers: [{ id: "self", url: home.http }, frankfurt],
    config: {
      ...combined,
      transports: { throughputTarget: "auto", latencyTarget: "auto" },
    },
  });
  await ready(page);
  const saved = await run(page);
  expect(saved.multiServer?.failures).toEqual([]);
  const [self, peer] = saved.multiServer!.servers;
  expect(self.throughput?.origin).toBe(home.http);
  expect(peer.throughput?.origin).toBe(frankfurt.url);
  expect(self.latencyTarget?.transport).toBe("websocket");
  expect(peer.latencyTarget).toBeNull();

  await page.getByRole("button", { name: "Toggle Details" }).click();
  const info = page.locator(".infra");
  const badge = (role: string) =>
    info.locator(".path", { hasText: `${role} path` }).locator("mark");
  await info.getByRole("combobox", { name: "Inspect server" }).fill("server-1");
  await expect(info.locator(".server-card")).toContainText(frankfurt.url);
  await expect(badge("throughput")).toHaveText("Used");
  await expect(badge("latency")).toHaveText("Not in test");
  await info.getByRole("combobox", { name: "Inspect server" }).fill("self");
  await expect(badge("latency")).toHaveText("Used");
});

test("deselecting a verified peer starts a self-only run at once", async (page) => {
  await open(page, home.url, { servers: [home, frankfurt] });
  await ready(page);
  const settings = await openSettings(page);
  await settings.getByRole("checkbox", { name: /^Frankfurt/ }).click();
  await closeSettings(page);
  const saved = await run(page);
  expect(saved.multiServer?.participants).toEqual(["self"]);
  expect(saved.multiServer?.failures).toEqual([]);
  expect(saved.stages.upload.result?.reportedBytesPerSec).toBeGreaterThan(0);
});
