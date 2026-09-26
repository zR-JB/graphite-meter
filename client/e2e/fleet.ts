import { HISTORY_DB } from "../src/lib/history/dbSchema";
import { incoherence, type HistoryRecord } from "../src/lib/history/types";
import type { RunnerConfig } from "../src/lib/runner/contract";
import { describe, launch, type Server } from "./servers";
import { expect, type Page } from "./webview";

const env = JSON.parse(process.env.GM_E2E ?? '{ "fleet": [] }');
export const fleet: Server[] = env.fleet;
export const [home, frankfurt, amsterdam, helsinki, locked] = fleet;
export const password: string = env.password;
export const harness: string = env.harness;

export async function spawnPeer(name: string, env = {}) {
  const server = await describe(name.toLowerCase(), name);
  const child = await launch(
    JSON.parse(process.env.GM_E2E_LAUNCH!),
    server,
    env,
  );
  return Object.assign(child, { server });
}

export const baseConfig = {
  transports: {
    throughputTarget: "auto",
    latencyTarget: "transport:websocket",
  },
  stages: { latency: true, download: true, upload: true, bidirectional: false },
  duration: {
    warmupMs: 0,
    latencyMs: 1000,
    downloadMs: 1000,
    uploadMs: 1000,
    bidirectionalMs: 1000,
  },
  adaptive: { enabled: false },
  transferStreams: { mode: "forced", count: 1 },
};
export interface Seed {
  servers?: Pick<Server, "id" | "url">[];
  config?: Partial<Record<keyof RunnerConfig, unknown>>;
  latency?: { mode: "all" | "primary"; serverId: string };
}

export async function open(page: Page, origin = home.url, seed: Seed = {}) {
  await page.cdp("Network.clearBrowserCookies");
  for (const origin of fleet.flatMap((server) => [server.http, server.url]))
    await page.cdp("Storage.clearDataForOrigin", {
      origin,
      storageTypes: "local_storage,indexeddb",
    });
  await page.addInitScript(
    ({ servers, config, latency }) => {
      if (localStorage.getItem("graphite-meter:v1")) return;
      if (servers)
        localStorage.setItem(
          "graphite-meter:server-selection:v1",
          JSON.stringify(servers),
        );
      localStorage.setItem(
        "graphite-meter:v1",
        JSON.stringify({
          config,
          latencySelection: latency,
          resultHistoryPreference: "enabled",
        }),
      );
    },
    {
      servers: seed.servers?.map(({ id, url }) => ({ id, url })),
      config: { ...baseConfig, ...seed.config },
      latency: seed.latency ?? { mode: "primary", serverId: "self" },
    },
  );
  await page.goto(origin);
}

export async function openSettings(page: Page) {
  const panel = page.locator('[aria-label="Settings"]');
  if (await panel.all((els) => els.every((el) => el.inert)))
    await page.getByRole("button", { name: "Open settings" }).click();
  await expect
    .poll(() => panel.all((els) => els.some((el) => !el.inert)))
    .toBe(true);
  return panel;
}

export async function closeSettings(page: Page) {
  const panel = page.locator('[aria-label="Settings"]');
  await panel.getByRole("button", { name: "Close Settings" }).click();
}

export async function ready(page: Page) {
  const settings = await openSettings(page);
  await expect(settings.locator('[data-readiness="verified"]')).toBeVisible({
    timeout: 15_000,
  });
  await closeSettings(page);
}

export const runButton = (page: Page, name: string | RegExp) =>
  page.getByRole("button", { name, exact: true });

export const phase = (page: Page, name: string) =>
  page.locator(`#console[data-phase="${name}"]`);

export async function run(page: Page, timeout = 20_000) {
  const startedAt = Date.now();
  await runButton(page, /^(Start test|Run again)$/).click();
  const saved = await savedResult(page, startedAt, timeout);
  await expect(page.locator('#console[data-phase="complete"]')).toHaveCount(1);
  return saved;
}

export async function savedResult(page: Page, after = 0, timeout = 10_000) {
  let record: HistoryRecord | undefined;
  await expect
    .poll(
      async () => {
        record = await page.evaluate(readLatest, HISTORY_DB);
        return (record?.completedAt ?? -1) >= after;
      },
      { timeout },
    )
    .toBe(true);
  expect(incoherence(record!)).toEqual([]);
  return record!;
}

async function readLatest(schema: typeof HISTORY_DB) {
  const names = (await indexedDB.databases()).map((db) => db.name);
  if (!names.includes(schema.name)) return undefined;
  return new Promise<HistoryRecord | undefined>((resolve, reject) => {
    const request = indexedDB.open(schema.name);
    request.onerror = () => reject(request.error);
    request.onsuccess = () => {
      const db = request.result;
      const all = db
        .transaction(schema.resultsStore)
        .objectStore(schema.resultsStore)
        .getAll();
      all.onsuccess = () => {
        db.close();
        resolve(all.result.sort((a, b) => b.completedAt - a.completedAt)[0]);
      };
      all.onerror = () => reject(all.error);
    };
  });
}
