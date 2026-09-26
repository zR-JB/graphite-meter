import { HISTORY_DB } from "../src/lib/history/dbSchema";
import type { HistoryRecord } from "../src/lib/history/types";
import { home, open, ready, run, runButton } from "./fleet";
import { expect, test, type Page } from "./webview";

const id = (index: number) =>
  `00000000-0000-4000-8000-${index.toString(16).padStart(12, "0")}`;
const base = Date.UTC(2026, 7, 28, 12);

function record(index: number, completedAt = base - index * 60_000) {
  const lane = (rate: number) => ({
    reportedBytesPerSec: rate,
    peakBytesPerSec: rate * 1.08,
    fullAverageBytesPerSec: rate * 0.96,
    method: "full-average" as const,
    totalBytes: 9_999_999_999,
    stabilityPct: 4,
    probeTimeoutPct: 0.2,
    stabilityScore: 0.94,
    band: "high" as const,
    serverAuthoritative: true,
  });
  const latency = {
    min: 9.1,
    max: 38.4,
    p10: 10.2,
    p90: 24.8,
    center: 14.6,
    jitter: 2.7,
    timeoutRatio: 0.01,
    accountingComplete: true,
    timeoutCount: 1,
    unresolvedCount: 0,
    sendFailureCount: 0,
    count: 100,
  };
  const value: HistoryRecord = {
    schemaVersion: 4,
    id: id(index),
    startedAt: completedAt - 65_432,
    completedAt,
    durationMs: 65_432,
    stages: {
      latency: {
        status: "complete",
        result: {
          reportedMs: 12.4,
          jitterMs: 2.2,
        },
        lanes: {
          latency,
          download: latency,
          upload: latency,
          bidirectional: null,
        },
      },
      download: { status: "complete", result: lane(1_000_000 + index) },
      upload: { status: "complete", result: lane(61_500_000) },
      bidirectional: { status: "not-run", down: null, up: null },
    },
    bufferbloat: { idleMs: 12.4, loadedMs: 23.9, increaseMs: 11.5, grade: "A" },
    totalBytes: 39_999_999_996,
    server: { name: "Archive fixture", location: "Loopback", engine: "e2e" },
    transport: {
      throughput: { protocol: "h3", kind: "webtransport" },
      latency: { protocol: "h3", kind: "webtransport" },
    },
    ipVersion: 6,
    client: { build: "e2e" },
    wireEstimates: null,
  };
  return value;
}

interface Archive {
  records: unknown[];
  clears?: unknown;
  version?: number;
}

// Writes raw rows as another tab or an older build would have left them.
function seed(page: Page, archive: Archive) {
  return page.evaluate(
    ({ db, records, clears, version }) =>
      new Promise<void>((resolve, reject) => {
        const opening = indexedDB.open(db.name, version ?? db.version);
        opening.onupgradeneeded = () => {
          const database = opening.result;
          database
            .createObjectStore(db.resultsStore, { keyPath: db.resultKeyPath })
            .createIndex(db.completedAtIndex, db.completedAtIndex);
          if (version === undefined)
            database.createObjectStore(db.metadataStore, {
              keyPath: db.metadataKeyPath,
            });
        };
        opening.onerror = () => reject(opening.error);
        opening.onsuccess = () => {
          const database = opening.result;
          const stores = [...database.objectStoreNames];
          const tx = database.transaction(stores, "readwrite");
          for (const value of records)
            tx.objectStore(db.resultsStore).put(value);
          if (clears !== undefined)
            tx.objectStore(db.metadataStore).put({
              key: db.clearsKey,
              value: clears,
            });
          tx.oncomplete = () => {
            database.close();
            resolve();
          };
          tx.onerror = () => reject(tx.error);
        };
      }),
    { db: HISTORY_DB, ...archive },
  );
}

function stored(page: Page) {
  return page.evaluate(
    (db) =>
      new Promise<Archive>((resolve, reject) => {
        const opening = indexedDB.open(db.name);
        opening.onerror = () => reject(opening.error);
        opening.onsuccess = () => {
          const database = opening.result;
          const stores = [...database.objectStoreNames];
          const tx = database.transaction(stores);
          const rows = tx.objectStore(db.resultsStore).getAll();
          const meta = stores.includes(db.metadataStore)
            ? tx.objectStore(db.metadataStore).get(db.clearsKey)
            : null;
          tx.oncomplete = () => {
            database.close();
            resolve({
              records: rows.result,
              clears: meta?.result?.value,
              version: database.version,
            });
          };
        };
      }),
    HISTORY_DB,
  );
}

const fixturePage = (page: Page) => open(page, `${home.url}/version.json`);
async function history(page: Page, selected = "") {
  await page.goto(`${home.url}/#/history${selected && `/${selected}`}`);
}

test("a 2,000-result archive sorts in bounded chunks and caps deep links", async (page) => {
  await fixturePage(page);
  const archive = Array.from({ length: 2_001 }, (_, index) => record(index));
  await seed(page, { records: archive });
  await page.setViewportSize({ width: 1366, height: 768 });
  await history(page);
  const rows = page.locator(".result-row");
  await expect(rows).toHaveCount(50, { timeout: 15_000 });
  const heading = page.locator(".history-head p");
  await expect(heading).toContainText("2000 results");
  const elapsed = await page.evaluate((lowest) => {
    const button = document.querySelector<HTMLButtonElement>(
      '.column-head button:has([data-tone="download"])',
    )!;
    const list = document.querySelector(".history-table ol")!;
    const started = performance.now();
    return new Promise<number>((resolve) => {
      const done = () => {
        const first = document.querySelector(".result-row");
        if (first?.getAttribute("data-history-id") !== lowest) return;
        observer.disconnect();
        resolve(performance.now() - started);
      };
      const observer = new MutationObserver(done);
      observer.observe(list, { childList: true, subtree: true });
      button.click();
      queueMicrotask(done);
    });
  }, id(1_999));
  expect(elapsed).toBeLessThan(1_000);
  await expect(rows).toHaveCount(50);

  await history(page, id(2_000));
  await expect(page.locator(".result-detail")).toBeVisible({ timeout: 15_000 });
  await expect(heading).toContainText("2000 results");
});

test("unsupported and malformed rows are skipped, kept and clearable", async (page) => {
  await fixturePage(page);
  const current = record(1);
  const old = [
    { ...record(2), schemaVersion: 1 },
    { ...record(3), schemaVersion: 2 },
    record(4, 1e20),
  ];
  await seed(page, { records: [current, ...old] });
  const before = await stored(page);
  await history(page);
  await expect(page.locator(".result-row")).toHaveCount(1);
  await expect(page.locator(".history-workspace")).toContainText(
    "3 unsupported or malformed records were ignored.",
  );
  await history(page, old[0].id);
  await expect(
    page.getByRole("heading", { name: "Unreadable saved result" }),
  ).toBeVisible();
  await page.reload();
  await expect(
    page.getByRole("heading", { name: "Unreadable saved result" }),
  ).toBeVisible();
  expect(await stored(page)).toEqual(before);

  const management = page.getByRole("button", { name: "History actions" });
  await management.click();
  await page.getByRole("menuitem", { name: /Clear all saved results/ }).click();
  await page
    .getByRole("alertdialog", { name: "Clear result history?" })
    .getByRole("button", { name: "Clear history" })
    .click();
  await expect(
    page.getByRole("heading", { name: "No saved results" }),
  ).toBeVisible();
  expect((await stored(page)).records).toEqual([]);
});

test("a save ignores corrupt clear metadata, keeps raw rows, and trusts clears over the clock", async (page) => {
  await fixturePage(page);
  const malformed = { id: id(9), completedAt: base, unexpected: "raw row" };
  await seed(page, {
    records: [record(1), malformed],
    clears: { corrupt: true },
  });
  await page.goto(home.url);
  await page.evaluate(() => {
    (window as any).saves = 0;
    new BroadcastChannel("graphite-meter-history").onmessage = () => {
      (window as any).saves++;
    };
  });
  await ready(page);
  await run(page);
  const saved = await stored(page);
  expect(saved.records).toHaveLength(3);
  expect(saved.records).toContainEqual(malformed);
  expect(await page.evaluate(() => (window as any).saves)).toBe(1);
  // A result after another tab's clear saves even when the clock stepped back a day.
  await seed(page, { records: [], clears: 2 });
  await page.reload();
  await page.evaluate(() => {
    const now = Date.now;
    Date.now = () => now() - 86_400_000;
  });
  await ready(page);
  await runButton(page, /^(Start test|Run again)$/).click();
  await expect
    .poll(async () => (await stored(page)).records.length, { timeout: 20_000 })
    .toBe(4);
  await history(page);
  await expect(page.locator(".result-row")).toHaveCount(3);
  await expect(page.locator(".history-workspace")).toContainText(
    "1 unsupported or malformed record was ignored.",
  );
});

test("History refuses other database versions without changing them", async (page) => {
  for (const version of [1, HISTORY_DB.version + 1]) {
    await fixturePage(page);
    await seed(page, { records: [{ id: "preserved" }], version });
    const before = await stored(page);
    await history(page);
    const refusal = page.getByRole("heading", {
      name: "History is unavailable",
    });
    await expect(refusal).toBeVisible();
    await page.getByRole("button", { name: "Retry", exact: true }).click();
    await expect(refusal).toBeVisible();
    expect(await stored(page)).toEqual(before);
  }
});
