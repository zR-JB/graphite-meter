import { HISTORY_DB } from "../src/lib/history/dbSchema";
import { countSaves, home, open, ready, run, runButton } from "./fleet";
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
  return {
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
    failures: [],
    wireEstimates: null,
  };
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
        opening.onblocked = () => reject(new Error("seeding is blocked"));
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
          tx.onerror = tx.onabort = () =>
            reject(tx.error ?? new Error("seed aborted"));
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
        opening.onblocked = () => reject(new Error("reading is blocked"));
        opening.onupgradeneeded = () => {
          opening.transaction?.abort();
          reject(new Error("reading found no database"));
        };
        opening.onsuccess = () => {
          const database = opening.result;
          const stores = [...database.objectStoreNames];
          let tx: IDBTransaction;
          try {
            tx = database.transaction(stores);
          } catch (error) {
            database.close();
            return reject(error);
          }
          tx.onerror = tx.onabort = () =>
            reject(tx.error ?? new Error("read aborted"));
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

const OPEN_BOUND = { timeout: 10_000 };

test("closing History stops an unfinished archive scan and preserves its records", async (page) => {
  await fixturePage(page);
  await seed(page, {
    records: Array.from({ length: 2000 }, (_, i) => record(i)),
  });
  await page.goto(home.url);
  await expect(page.locator("#console")).toHaveCount(1);
  await page.evaluate((db) => {
    const stats = { deliveries: 0, dismissed: false };
    const original = IDBCursor.prototype.continue;
    IDBCursor.prototype.continue = function (...args) {
      if (
        this.source instanceof IDBIndex &&
        this.source.objectStore.name === db.resultsStore &&
        this.source.objectStore.transaction.mode === "readonly"
      ) {
        stats.deliveries++;
        // Interrupt the actual scan, without slowing cursor deliveries or
        // relying on the test driver to catch a brief intermediate state.
        if (stats.deliveries === 20) {
          const close = document.querySelector<HTMLButtonElement>(
            'button[aria-label="Close History"]',
          );
          if (!close) throw new Error("History scan has no dismiss control");
          stats.dismissed = true;
          close.click();
        }
      }
      return original.apply(this, args);
    };
    Object.assign(window, {
      archiveScan: stats,
      originalCursorContinue: original,
    });
  }, HISTORY_DB);
  await page.getByRole("button", { name: "History", exact: true }).click();
  await expect.poll(() => page.evaluate(() => location.hash)).toBe("#/");
  await expect(page.locator(".history-workspace")).toHaveCount(0);
  const stopped = await page.evaluate(() => (window as any).archiveScan);
  expect(stopped.dismissed).toBe(true);
  expect(stopped.deliveries).toBeGreaterThanOrEqual(20);
  expect(stopped.deliveries).toBeLessThan(250);
  await Bun.sleep(200);
  expect(
    await page.evaluate(() => (window as any).archiveScan.deliveries),
  ).toBe(stopped.deliveries);
  await page.evaluate(() => {
    IDBCursor.prototype.continue = (window as any).originalCursorContinue;
  });
  await page.getByRole("button", { name: "History", exact: true }).click();
  await expect(page.locator(".result-row")).toHaveCount(50, OPEN_BOUND);
  expect((await stored(page)).records).toHaveLength(2000);
});

test("a 2,000-result archive sorts in bounded chunks and caps deep links", async (page) => {
  await fixturePage(page);
  const archive = Array.from({ length: 2_001 }, (_, index) => record(index));
  await seed(page, { records: archive });
  await page.setViewportSize({ width: 1366, height: 768 });
  await history(page);
  const rows = page.locator(".result-row");
  await expect(rows).toHaveCount(50, { timeout: 15_000 });
  const heading = page.locator(".history-head .head-facts dd").nth(0);
  await expect(heading).toHaveText("2000");
  await page.locator('.column-head button:has([data-tone="download"])').click();
  await expect
    .poll(() =>
      page.evaluate(() =>
        document.querySelector(".result-row")?.getAttribute("data-history-id"),
      ),
    )
    .toBe(id(1_999));
  await expect(rows).toHaveCount(50);

  await history(page, id(2_000));
  await expect(page.locator(".result-detail")).toBeVisible({ timeout: 15_000 });
  expect(await page.locator(".result-detail").textContent()).not.toContain(
    "Grade",
  );
  await expect(heading).toHaveText("2000");
});

test("a closed Columns popover never takes a tap meant for a result", async (page) => {
  await fixturePage(page);
  await seed(page, { records: [record(1)] });
  await page.setViewportSize({ width: 390, height: 844 });
  await history(page);
  const options = page.getByRole("dialog", { name: "History view options" });
  await page
    .getByRole("button", { name: "Choose columns and sort order" })
    .click();
  await expect(options).toBeVisible();
  await page.press("Escape");
  await page.locator(".result-row").click();
  await expect(page.locator(".result-detail")).toBeVisible();
});

test("the keyboard moves the list's split within both panes' limits, and it survives a reload", async (page) => {
  await fixturePage(page);
  await seed(page, { records: [record(1)] });
  await page.setViewportSize({ width: 1280, height: 800 });
  await history(page, id(1));
  const handle = page.getByRole("slider", { name: /^Resize results list/ });
  const list = () =>
    page.locator(".history-list").evaluate((el: HTMLElement) => el.offsetWidth);
  await expect(handle).toHaveAttribute("aria-valuenow", "538");
  await handle.evaluate((el: HTMLElement) => el.focus());
  // The list never narrows past 360 px, nor leaves the detail under 460 px.
  for (const [key, width] of [
    ["ArrowRight", 554],
    ["Home", 360],
    ["End", 820],
    ["Enter", 538],
    ["ArrowLeft", 522],
  ] as const) {
    await page.press(key);
    await expect.poll(list).toBe(width);
  }
  await expect(handle).toHaveAttribute("aria-valuenow", "522");
  await expect
    .poll(() =>
      page.evaluate(
        () =>
          JSON.parse(localStorage.getItem("graphite-meter:v1")!).historySplit,
      ),
    )
    .toBe(522 / 1280);
  await page.reload();
  await expect.poll(list).toBe(522);
  // A narrower window keeps the saved share within the limits, then shows one pane.
  await page.setViewportSize({ width: 900, height: 800 });
  await expect.poll(list).toBe(367);
  await page.setViewportSize({ width: 800, height: 800 });
  await expect(handle).toHaveCount(0);
});

test("unsupported and malformed rows are skipped, kept and clearable", async (page) => {
  await fixturePage(page);
  const current = record(1);
  const old = [
    { ...record(2), schemaVersion: 1 },
    { ...record(3), schemaVersion: 2 },
    record(4, 1e20),
  ];
  const broken = record(5);
  Object.assign(broken.stages.latency.result, { reportedMs: "fast" });
  await seed(page, { records: [current, broken, ...old] });
  const before = await stored(page);
  await history(page);
  await expect(page.locator(".result-row")).toHaveCount(1);
  await expect(page.locator(".history-workspace")).toContainText(
    "4 unsupported or malformed records were ignored.",
  );
  const unreadable = page.getByRole("heading", {
    name: "Unreadable saved result",
  });
  for (const id of [broken.id, old[0].id]) {
    await history(page, id);
    await expect(unreadable).toBeVisible();
  }
  await page.reload();
  await expect(unreadable).toBeVisible();
  expect(await stored(page)).toEqual(before);

  const management = page.getByRole("button", { name: "History actions" });
  await management.click();
  await page.getByRole("menuitem", { name: "Clear history" }).click();
  await page
    .getByRole("alertdialog", { name: "Clear history?" })
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
  const saves = await countSaves(page);
  await ready(page);
  await run(page);
  const saved = await stored(page);
  expect(saved.records).toHaveLength(3);
  expect(saved.records).toContainEqual(malformed);
  expect(await saves()).toBe(1);
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

test("History never waits on another version's connection and never changes its database", async (page) => {
  const refusal = page.getByRole("heading", { name: "History is unavailable" });
  await fixturePage(page);
  await seed(page, { records: [record(1)] });
  await history(page);
  await expect(page.locator(".result-row")).toHaveCount(1);
  // A newer build's upgrade gets the database at once, and History then refuses it.
  const upgrade = await page.evaluate(
    (db) =>
      new Promise<string>((resolve) => {
        const opening = indexedDB.open(db.name, db.version + 1);
        opening.onblocked = () => resolve("blocked");
        opening.onerror = () => resolve(`failed: ${opening.error?.name}`);
        opening.onsuccess = () => {
          opening.result.close();
          resolve("upgraded");
        };
      }),
    HISTORY_DB,
  );
  expect(upgrade).toBe("upgraded");
  await page.evaluate(() =>
    new BroadcastChannel("graphite-meter-history").postMessage(""),
  );
  await expect(refusal).toBeVisible(OPEN_BOUND);

  // An older build that keeps its connection open is refused within a moment, and never upgraded.
  await fixturePage(page);
  await seed(page, { records: [{ id: "preserved" }], version: 1 });
  const before = await stored(page);
  await page.goto(home.url);
  const older = await page.evaluate(
    (db) =>
      new Promise<string>((resolve) => {
        const opening = indexedDB.open(db.name);
        opening.onblocked = () => resolve("blocked");
        opening.onerror = () => resolve(`failed: ${opening.error?.name}`);
        opening.onsuccess = () => {
          (window as any).older = opening.result;
          resolve(`v${opening.result.version}`);
        };
      }),
    HISTORY_DB,
  );
  expect(older).toBe("v1");
  await page.evaluate(() => (location.hash = "#/history"));
  await expect(refusal).toBeVisible(OPEN_BOUND);
  await page.evaluate(() => (window as any).older.close());
  // An open cannot be cancelled: read once History's queued upgrade has been refused and left version 1.
  await expect
    .poll(
      async () => {
        const { databases, opens } = await page.storage();
        const settled = /^(success|error)/;
        const pending = opens.filter(
          ({ events }) => !events.some((event) => settled.test(event)),
        );
        return { databases, pending };
      },
      { timeout: 10_000 },
    )
    .toEqual({
      databases: [{ name: HISTORY_DB.name, version: 1 }],
      pending: [],
    });
  expect(await stored(page)).toEqual(before);
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
    await expect(refusal).toBeVisible(OPEN_BOUND);
    await page.getByRole("button", { name: "Retry", exact: true }).click();
    // The refusal shows before an upgrade's abort settles, and was already shown before Retry. Chrome 153's
    // renderer crashes when an open races an aborting upgrade, so read once both of History's opens settled.
    await expect
      .poll(async () => {
        const { opens } = await page.storage();
        const history = opens.filter((o) => o.version === HISTORY_DB.version);
        const settled = history.filter(({ events }) =>
          events.some((event) => /^(success|error)/.test(event)),
        );
        return { opened: history.length, settled: settled.length };
      }, OPEN_BOUND)
      .toEqual({ opened: 2, settled: 2 });
    await expect(refusal).toBeVisible(OPEN_BOUND);
    expect(await stored(page)).toEqual(before);
  }
});
