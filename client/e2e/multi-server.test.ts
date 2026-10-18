import { readHistoryRecord } from "../src/app/history/records";
import type { ServerMeasurementSummary } from "../src/engine/run/result";
import {
  baseConfig,
  catalog,
  countSaves,
  frankfurt,
  home,
  open,
  phase,
  ready,
  run,
  runButton,
  savedResult,
  spawnPeer,
} from "./fleet";
import type { Server } from "./servers";
import { browser, expect, test, type Page } from "./webview";

export const combined = {
  ...baseConfig,
  // Firefox can starve both servers' loopback upload POSTs for a few hundred ms, which 1 s cannot spare.
  duration: {
    ...baseConfig.duration,
    downloadMs: 1000,
    uploadMs: browser === "firefox" ? 2000 : 1000,
  },
};

// At GB/s Firefox's one network thread starves all but one loopback server; a 200 Mbit/s link shares evenly.
const LINK_BYTES_PER_SEC = 25e6;

test("four servers share one run and keep separate receiver windows", async (page) => {
  const peers = await Promise.all(
    ["Frankfurt", "Amsterdam", "Helsinki"].map((name) => spawnPeer(name, {}, LINK_BYTES_PER_SEC)),
  );
  const self = await spawnPeer(
    "Home",
    catalog(...peers.map(({ server }) => server)),
    LINK_BYTES_PER_SEC,
  );
  try {
    await shareOneRun(page, [{ ...self.server, id: "self" }, ...peers.map(({ server }) => server)]);
  } finally {
    for (const server of [self, ...peers]) server.kill();
  }
});

async function shareOneRun(page: Page, four: Server[]) {
  await open(page, four[0]!.url, {
    servers: four.map(({ id, url }) => ({ id, url })),
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
  expect(readHistoryRecord(saved)).toEqual(saved);
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
  await scope.fill(four[1]!.id);
  await expect(scope).toHaveValue(four[1]!.id);
  expect(await savedResult(page)).toEqual(saved);

  await page.evaluate((id) => (location.hash = `/history/${id}`), saved.id);
  await page.reload();
  const facts = page.locator(".result-detail section.group", { hasText: "Address" });
  const picker = facts.getByRole("combobox", { name: "Server", exact: true });
  await expect(picker.locator("option")).toHaveCount(4);
  await picker.fill(four[1]!.id);
  await expect(facts).toContainText(new URL(four[1]!.url).host);
  expect(await savedResult(page)).toEqual(saved);
}

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
  const [self, peer] = saved.result.multiServer.servers as [
    ServerMeasurementSummary,
    ServerMeasurementSummary,
  ];
  expect(self.throughput?.origin).toBe(home.http);
  // Automatic falls back past a slow HTTP/1.1 probe, so Frankfurt may be measured through any of its origins.
  const origins = [frankfurt.url, frankfurt.http, frankfurt.h2, frankfurt.h3];
  expect(origins).toContain(peer.throughput?.origin);
  expect(self.latencyTarget?.transport).toBe("websocket");
  expect(peer.latencyTarget?.transport).toBe("websocket");

  await page.getByRole("button", { name: "Details" }).click();
  const info = page.locator(".infra");
  const path = (role: string) => info.locator(`.path[data-role="${role}"] dd`);
  await info.getByRole("combobox", { name: "Server", exact: true }).fill("server-1");
  await expect(info.locator(".server-card")).toContainText(frankfurt.url);
  await expect(path("throughput")).toContainText("Used");
  await expect(path("latency")).toContainText("Used");
});

test("the lanes switch servers mid-run and Details inspects another without moving them", async (page) => {
  await open(page, home.url, {
    servers: [home, frankfurt],
    config: {
      ...combined,
      duration: { ...combined.duration, latencyMs: 1500, downloadMs: 2000 },
    },
  });
  await ready(page);
  await runButton(page, "Start test").click();
  await expect(phase(page, "download")).toHaveCount(1);
  const lanes = page.getByRole("combobox", { name: "Show latency to" });
  await lanes.fill("server-1");
  await expect(lanes).toHaveValue("server-1");
  await page.getByRole("button", { name: "Details" }).click();
  const inspect = page.locator(".infra").getByRole("combobox", { name: "Server", exact: true });
  await expect(inspect).toBeEnabled();
  await inspect.fill("self");
  await expect(page.locator(".infra .server-card")).toContainText(home.url);
  await expect(lanes).toHaveValue("server-1");
  await expect(phase(page, "complete")).toHaveCount(1, { timeout: 20_000 });
  await expect(lanes).toHaveValue("server-1");
});

test("a missed final upload checkpoint is retried, and the lens and the dial read each server's saved latency", async (page) => {
  await open(page, home.url, { servers: [home, frankfurt], config: combined });
  await ready(page);
  // Automatic falls back past a slow HTTP/1.1 probe, so Frankfurt's checkpoints may go to any of its origins.
  const origins = [frankfurt.url, frankfurt.h2, frankfurt.h3];
  await page.evaluate((origins) => {
    const original = window.fetch.bind(window);
    Object.assign(window, { checkpoints: 0 });
    window.fetch = ((input: RequestInfo | URL, init?: RequestInit) => {
      const url = new URL(String(input), location.href);
      const uploading = document.querySelector<HTMLElement>("#console")?.dataset.phase === "upload";
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
  const saves = await countSaves(page);
  const saved = await run(page);
  expect(await page.evaluate(() => (window as any).checkpoints)).toBeGreaterThan(1);
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
  expect(await saves()).toBe(1);
  expect(saved.result.multiServer.latencyFocus).toBe("self");
  for (const server of saved.result.multiServer.servers)
    expect(server.latencyByStage.latency?.probeCount).toBeGreaterThan(0);
  const lens = page.getByRole("combobox", {
    name: "Servers shown in the results",
  });
  const picker = page.getByRole("combobox", { name: "Show latency to" });
  const latency = page.getByRole("region", {
    name: "Latency, jitter and probe timeouts by stage",
  });
  const figure = (selector: string) =>
    page.evaluate(
      (selector: string) =>
        Number(document.querySelector(selector)?.textContent?.match(/[\d.]+/)?.[0] ?? Number.NaN),
      selector,
    );
  const dial = () => figure(".hero .terminal-readout");
  const download = () => figure('.results .card[data-tone="download"] .headline');
  const latencyCard = () =>
    page
      .locator('.results .card[data-tone="latency"]')
      .evaluate((card: Element) =>
        [card.querySelector(".headline"), ...card.querySelectorAll(".facts dd")]
          .map((el) => el?.textContent)
          .join(" | "),
      );
  await expect(lens).toHaveValue("");
  await expect(latency).toContainText(home.name);
  await lens.fill("server-1");
  await expect(lens).toHaveValue("server-1");
  await expect(latency).toContainText(frankfurt.name);
  await expect.poll(async () => (await dial()) === (await download())).toBe(true);
  const alone = await latencyCard();
  // Every server together again, latency shown for the second: the card is the second's, headline and spread.
  await lens.fill("");
  await picker.fill("server-1");
  await expect.poll(latencyCard).toBe(alone);
  expect(await savedResult(page)).toEqual(saved);
  expect(await saves()).toBe(1);

  await page.evaluate((id) => (location.hash = `/history/${id}`), saved.id);
  await page.reload();
  await expect(page.locator(".result-detail")).toBeVisible();
  expect(await savedResult(page)).toEqual(saved);
});
