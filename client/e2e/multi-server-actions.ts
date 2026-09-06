import { Page, openSettings, expect } from "../browser/webview";
import { HISTORY_DB } from "../src/lib/history/dbSchema";
import type { HistoryRecord } from "../src/lib/history/types";
import type { RunnerConfig } from "../src/lib/runner/contract";
import { fleet } from "./multi-server-fixtures";

export async function configure(
  page: Page,
  ids: string[],
  duration = 1500,
  config: Partial<RunnerConfig> = {},
  latencySelection: { mode: "all" | "primary"; serverId: string } = {
    mode: "all",
    serverId: "self",
  },
  pageOrigin = fleet[0].url,
) {
  await page.addInitScript(
    ({ ids, servers, duration, config, latencySelection }) => {
      localStorage.setItem(
        "graphite-meter:server-selection:v1",
        JSON.stringify(
          servers
            .filter((server: { id: string }) => ids.includes(server.id))
            .map(({ id, url }: { id: string; url: string }) => ({ id, url })),
        ),
      );
      localStorage.setItem(
        "graphite-meter:v1",
        JSON.stringify({
          latencySelection,
          config: {
            transports: {
              throughputTarget: "auto",
              latencyTarget: "transport:websocket",
            },
            stages: {
              latency: true,
              download: true,
              upload: true,
              bidirectional: true,
            },
            duration: {
              warmupMs: 250,
              latencyMs: 1000,
              downloadMs: duration,
              uploadMs: duration,
              bidirectionalMs: duration,
            },
            adaptive: { enabled: false },
            transferStreams: { mode: "forced", count: 1 },
            ...config,
          },
          resultHistoryPreference: "enabled",
        }),
      );
    },
    {
      ids,
      servers: fleet.map((server) =>
        server.id === "self" ? { ...server, url: pageOrigin } : server,
      ),
      duration,
      config,
      latencySelection,
    },
  );
  await page.goto(pageOrigin);
  const settings = await openSettings(page);
  if (
    !(await settings
      .getByLabel("Save completed results on this device")
      .evaluate((input) => input.checked))
  )
    await settings
      .getByText("Save completed results on this device", { exact: true })
      .click();
  await settings.getByRole("button", { name: "Close Settings" }).click();
}

export async function savedResult(
  page: Page,
  completedAfter = 0,
): Promise<HistoryRecord> {
  const deadline = Date.now() + 10000;
  while (Date.now() < deadline) {
    const record = await page.evaluate(async (schema) => {
      if (!(await indexedDB.databases()).some((db) => db.name === schema.name))
        return undefined;
      return new Promise<HistoryRecord | undefined>((resolve, reject) => {
        const open = indexedDB.open(schema.name, schema.version);
        open.onerror = () => reject(open.error);
        open.onsuccess = () => {
          const db = open.result;
          const req = db
            .transaction(schema.resultsStore, "readonly")
            .objectStore(schema.resultsStore)
            .getAll();
          req.onerror = () => {
            db.close();
            reject(req.error);
          };
          req.onsuccess = () => {
            db.close();
            resolve(
              req.result.sort(
                (a: HistoryRecord, b: HistoryRecord) =>
                  b.completedAt - a.completedAt,
              )[0],
            );
          };
        };
      });
    }, HISTORY_DB);
    if (record && record.completedAt >= completedAfter) return record;
    await Bun.sleep(50);
  }
  throw new Error("The completed result was not persisted within ten seconds");
}
export async function ready(page: Page) {
  const settings = await openSettings(page);
  await expect(
    settings.locator('.readiness-badge[data-state="verified"]'),
  ).toBeVisible({ timeout: 15000 });
  await settings.getByRole("button", { name: "Close Settings" }).click();
}
