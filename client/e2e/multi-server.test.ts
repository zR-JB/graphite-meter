import { readHistoryRecord } from "../src/lib/history/types";
import {
  amsterdam,
  baseConfig,
  closeSettings,
  countSaves,
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
    config: {
      ...combined,
      stages: { ...baseConfig.stages, bidirectional: true },
      // Eight transfers start at once, and a receiver's window opens only once every server has reported: in a
      // 1 s stage the slowest left under the 800 ms evidence floor on a loaded runner.
      duration: { ...combined.duration, bidirectionalMs: 2000 },
    },
  });
  await ready(page);
  const saved = await run(page);
  expect(readHistoryRecord(saved)).toBe(saved);
  expect(saved.result.outcome).toBe("complete");
  expect(saved.result.multiServer.failures).toEqual([]);
  expect(saved.result.multiServer.participants).toEqual(four.map((s) => s.id));
  for (const stage of ["download", "upload", "bidirectional"] as const) {
    const interval = saved.result.multiServer.intervals.find(
      (candidate) => candidate.stage === stage,
    )!;
    expect(interval.complete).toBe(true);
    expect(interval.participants).toHaveLength(4);
    const dirs = stage === "download" ? ["down"] : ["up"];
    for (const dir of stage === "bidirectional" ? ["down", "up"] : dirs)
      expect(interval.headline?.[dir as "down"]).toHaveLength(4);
  }
  for (const server of saved.result.multiServer.servers) {
    expect(server.latencyByStage.latency?.probeCount).toBeGreaterThan(0);
    expect(server.totalBytes.down).toBeGreaterThan(0);
    expect(server.totalBytes.up).toBeGreaterThan(0);
  }

  const scope = page.getByRole("combobox", {
    name: "Servers shown in the results",
  });
  await expect(scope).toBeVisible();
  await scope.fill("server-1");
  await expect(scope).toHaveValue("server-1");
  expect(await savedResult(page)).toEqual(saved);

  await page.evaluate((id) => (location.hash = `/history/${id}`), saved.id);
  await page.reload();
  const servers = page.locator(".result-detail section.group", {
    hasText: "Address",
  });
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
  expect(saved.result.multiServer.failures).toEqual([]);
  const [self, peer] = saved.result.multiServer.servers;
  expect(self.throughput?.origin).toBe(home.http);
  // Automatic falls back past a slow HTTP/1.1 probe, so Frankfurt may be measured through any of its origins.
  const origins = [frankfurt.url, frankfurt.http, frankfurt.h2, frankfurt.h3];
  expect(origins).toContain(peer.throughput?.origin);
  expect(self.latencyTarget?.transport).toBe("websocket");
  expect(peer.latencyTarget?.transport).toBe("websocket");

  await page.getByRole("button", { name: "Details" }).click();
  const info = page.locator(".infra");
  const path = (role: string) => info.locator(`.path[data-role="${role}"] dd`);
  await info.getByRole("combobox", { name: "Inspect server" }).fill("server-1");
  await expect(info.locator(".server-card")).toContainText(frankfurt.url);
  await expect(path("throughput")).toContainText("Used");
  await expect(path("latency")).toContainText("Used");
});

test("deselecting a verified peer starts a self-only run at once", async (page) => {
  await open(page, home.url, { servers: [home, frankfurt] });
  await ready(page);
  const settings = await openSettings(page);
  await settings.getByRole("checkbox", { name: /^Frankfurt/ }).click();
  await closeSettings(page);
  const saved = await run(page);
  expect(saved.result.multiServer.participants).toEqual(["self"]);
  expect(saved.result.multiServer.failures).toEqual([]);
  expect(saved.result.upload?.reportedBytesPerSec).toBeGreaterThan(0);
});

test("a missed final upload checkpoint is retried and keeps the interval and the run", async (page) => {
  await open(page, home.url, { servers: [home, frankfurt], config: combined });
  await ready(page);
  // Automatic falls back past a slow HTTP/1.1 probe, so Frankfurt's checkpoints may go to any of its origins.
  const origins = [frankfurt.url, frankfurt.h2, frankfurt.h3];
  await page.evaluate((origins) => {
    const original = window.fetch.bind(window);
    Object.assign(window, { checkpoints: 0 });
    window.fetch = ((input: RequestInfo | URL, init?: RequestInit) => {
      const url = new URL(String(input), location.href);
      const uploading =
        document.querySelector<HTMLElement>("#console")?.dataset.phase ===
        "upload";
      if (
        !uploading ||
        !origins.includes(url.origin) ||
        url.pathname !== "/upload/checkpoint" ||
        (window as any).checkpoints++
      )
        return original(input, init);
      return Promise.resolve(new Response(null, { status: 503 }));
    }) as typeof fetch;
  }, origins);
  const saved = await run(page);
  expect(
    await page.evaluate(() => (window as any).checkpoints),
  ).toBeGreaterThan(1);
  expect(saved.result.outcome).toBe("complete");
  expect(saved.result.multiServer.failures).toEqual([]);
  const upload = saved.result.multiServer.intervals.filter(
    (interval) => interval.stage === "upload",
  );
  expect(upload).toHaveLength(1);
  expect(upload[0]).toMatchObject({
    reason: "stage-start",
    complete: true,
    participants: ["self", "server-1"],
  });
  expect(saved.result.upload?.reportedBytesPerSec).toBeGreaterThan(0);
});

test("every server's latency is saved; the lens starts on the first selected and keeps the record", async (page) => {
  await open(page, home.url, { servers: [home, frankfurt], config: combined });
  await ready(page);
  const saves = await countSaves(page);
  const saved = await run(page);
  expect(await saves()).toBe(1);
  expect(saved.result.multiServer.latencyFocus).toBe("self");
  for (const server of saved.result.multiServer.servers)
    expect(server.latencyByStage.latency?.probeCount).toBeGreaterThan(0);
  const lens = page.getByRole("combobox", {
    name: "Servers shown in the results",
  });
  const latency = page.getByRole("region", {
    name: "Latency, jitter and probe timeouts by phase",
  });
  await expect(lens).toHaveValue("");
  await expect(latency).toContainText(home.name);
  await lens.fill("server-1");
  await expect(lens).toHaveValue("server-1");
  await expect(latency).toContainText(frankfurt.name);
  expect(await savedResult(page)).toEqual(saved);
  expect(await saves()).toBe(1);

  await page.evaluate((id) => (location.hash = `/history/${id}`), saved.id);
  await page.reload();
  await expect(page.locator(".result-detail")).toBeVisible();
  expect(await savedResult(page)).toEqual(saved);
});
