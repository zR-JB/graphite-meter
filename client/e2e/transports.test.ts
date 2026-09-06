import {
  configureSettings,
  openSettings,
  startTest,
  waitForCompletion,
} from "../browser/webview";
import { HISTORY_DB } from "../src/lib/history/dbSchema";
import { isHistoryRecord, type HistoryRecord } from "../src/lib/history/types";

import { test, expect, origins, harnessOrigin } from "./fixtures";
import type { CellSpec, CellResult } from "../bench/harness";
import type { Page } from "../browser/webview";
declare global {
  interface Window {
    __gmBench: { run: (spec: CellSpec) => Promise<CellResult> };
  }
}
/** Enough to prove lanes establish and carry, short enough to stay cheap. */
const WARMUP_MS = 250;
const MEASURE_MS = 750;
const fetchPaths = [
  { name: "HTTP/1.1 clear", origin: origins["h1-clear"], protocol: "http/1.1" },
  { name: "HTTP/1.1 TLS", origin: origins["h1-tls"], protocol: "http/1.1" },
  { name: "HTTP/2", origin: origins.h2, protocol: "h2" },
  { name: "HTTP/3", origin: origins.h3, protocol: "h3" },
];
const cells = [
  ...fetchPaths.map((path) => ({
    ...path,
    transport: "fetch-stream" as const,
  })),
  {
    name: "WebTransport streams",
    origin: origins.h3,
    protocol: "h3",
    transport: "webtransport" as const,
  },
  {
    name: "WebTransport datagrams",
    origin: origins.h3,
    protocol: "h3",
    transport: "webtransport-datagram" as const,
  },
];
async function runCell(
  page: Page,
  spec: Omit<CellSpec, "warmupMs" | "measureMs">,
): Promise<CellResult> {
  await page.goto(`${harnessOrigin}/bench/harness.html`);
  return page.evaluate((cell) => window.__gmBench.run(cell), {
    ...spec,
    warmupMs: WARMUP_MS,
    measureMs: MEASURE_MS,
  } as CellSpec);
}
for (const { name, origin, transport, protocol } of cells) {
  for (const dir of ["down", "up"] as const) {
    test(`${name} ${dir} carries bytes over the selected protocol`, async ({
      page,
    }) => {
      const result = await runCell(page, {
        origin,
        transport,
        dir,
        lanes: dir === "down" && transport === "fetch-stream" ? 2 : 1,
      });
      expect(result.errors).toEqual([]);
      expect(result.bytes).toBeGreaterThan(0);
      const observed = await page.evaluate(async (base) => {
        const response = await fetch(`${base}/probe`, { cache: "no-store" });
        if (!response.ok) throw new Error(`probe failed: ${response.status}`);
        return (await response.json()).protocolNegotiated;
      }, origin);
      expect(observed).toBe(protocol);
    });
  }
}

test("the served application measures real traffic and reloads its saved result", async ({
  page,
}) => {
  await page.setViewportSize({ width: 1280, height: 900 });
  const version = await fetch(`${origins["h1-clear"]}/version.json`).then(
    (response) => response.json(),
  );
  expect(version.label).toBe("prod");
  await page.goto(origins["h1-clear"]);
  const settings = await configureSettings(page, {
    "Warmup ms": "500",
    "Latency ms": "1500",
    "Download ms": "1500",
    "Upload ms": "1500",
  });
  for (const [label, enabled] of [
    ["Include concurrent download + upload", false],
    ["Finish stable stages early", false],
    ["Force exact stream count", true],
    ["Save completed results on this device", true],
  ] as const) {
    if (
      (await settings.getByLabel(label).evaluate((input) => input.checked)) !==
      enabled
    )
      await settings.getByText(label, { exact: true }).click();
  }
  await expect(
    settings.getByLabel("Streams per server and direction"),
  ).toBeVisible();
  await settings.getByLabel("Streams per server and direction").fill("1");
  // Keep this application smoke on the local HTTP/WebSocket path; the transport
  // matrix separately verifies TLS, multiplexed HTTP, and WebTransport workers.
  for (const role of ["throughput", "latency"])
    await settings
      .locator(
        `label:has(input[name="${role}-target"][value="${origins["h1-clear"]}"])`,
      )
      .click();
  await expect(
    settings.locator('.readiness-badge[data-state="verified"]'),
  ).toBeVisible({ timeout: 10_000 });
  await settings.getByRole("button", { name: "Close Settings" }).click();

  await startTest(page);
  await waitForCompletion(page, 20_000);
  await expect(page.locator(".result-card")).toHaveCount(3);
  await expect(page.locator(".result-card .partial")).toHaveCount(0);
  for (const label of ["Download", "Upload", "Ping"])
    await expect(
      page.locator(".result-card", { hasText: label }).locator(".val .num"),
    ).toHaveText(/\d/);

  const historySettings = await openSettings(page);
  await historySettings.getByRole("link", { name: "View History" }).click();
  await expect(
    page.getByRole("heading", { name: "History", exact: true }),
  ).toBeVisible();
  const row = page.locator("a.result-row");
  await expect(row).toHaveCount(1);
  const id = await row.getAttribute("data-history-id");
  expect(id).not.toBeNull();
  const saved = await page.evaluate(
    ({ schema, id }) =>
      new Promise<HistoryRecord>((resolve, reject) => {
        const opening = indexedDB.open(schema.name, schema.version);
        opening.onerror = () => reject(opening.error);
        opening.onsuccess = () => {
          const db = opening.result;
          const request = db
            .transaction(schema.resultsStore, "readonly")
            .objectStore(schema.resultsStore)
            .get(id);
          request.onerror = () => {
            db.close();
            reject(request.error);
          };
          request.onsuccess = () => {
            db.close();
            resolve(request.result);
          };
        };
      }),
    { schema: HISTORY_DB, id },
  );
  expect(isHistoryRecord(saved)).toBe(true);
  const preflight = await fetch(`${origins["h1-clear"]}/preflight`, {
    signal: AbortSignal.timeout(1_000),
  }).then((response) => response.json());
  expect(saved.server.name).toBe(preflight.server.name);
  expect(saved.server.engine).toBe(preflight.engineVersion);
  expect(saved.failures).toEqual([]);
  for (const stage of ["latency", "download", "upload"] as const)
    expect(saved.stages[stage].status).toBe("complete");
  expect(saved.stages.latency.lanes.latency?.count).toBeGreaterThan(0);
  for (const stage of ["download", "upload"] as const) {
    expect(saved.stages[stage].result?.totalBytes).toBeGreaterThan(0);
    expect(saved.stages[stage].result?.reportedBytesPerSec).toBeGreaterThan(0);
  }
  expect(saved.stages.upload.result?.serverAuthoritative).toBe(true);
  expect(saved.transport.throughput.kind).toBe("fetch-stream");
  expect(saved.transport.latency.kind).toBe("websocket");

  await page.reload();
  await expect(
    page.getByRole("heading", { name: "History", exact: true }),
  ).toBeVisible();
  await expect(page.locator("a.result-row")).toHaveCount(1);
  await expect(page.locator("a.result-row")).toHaveAttribute(
    "data-history-id",
    id!,
  );
});
