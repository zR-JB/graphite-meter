import type { HistoryRecord } from "../src/lib/history/types";
import {
  AxeBuilder,
  endpointPanel,
  expect,
  expectNoHorizontalOverflow,
  openApp,
  openEndpointInfo,
  openSettings,
  settingsPanel,
  startTest,
  test,
  waitForCompletion,
} from "./webview";
import { HISTORY_DB } from "../src/lib/history/dbSchema";
import {
  isHistoryGeneration,
  isRepairHistoryGeneration,
} from "../src/lib/history/changes";

type TestPage = Parameters<typeof openApp>[0];

const IDS = {
  newest: "00000000-0000-4000-8000-000000000127",
  middle: "00000000-0000-4000-8000-000000000126",
  oldest: "00000000-0000-4000-8000-000000000125",
};

function record(
  id: string,
  completedAt: number,
  download = 125_000_000,
): HistoryRecord {
  const lane = (rate: number) => ({
    meanBytesPerSec: rate,
    reportedBytesPerSec: rate,
    peakBytesPerSec: rate * 1.08,
    fullAverageBytesPerSec: rate * 0.96,
    method: "full-average" as const,
    totalBytes: 9_999_999_999,
    stabilityPct: 4,
    packetLossPct: 0.2,
    stabilityScore: 0.94,
    band: "high" as const,
    serverAuthoritative: true,
  });
  const latencyLane = {
    min: 9.1,
    max: 38.4,
    p10: 10.2,
    p90: 24.8,
    center: 14.6,
    jitter: 2.7,
    lossRatio: 0.01,
    count: 140,
  };
  return {
    schemaVersion: 1,
    id,
    startedAt: completedAt - 65_432,
    completedAt,
    durationMs: 65_432,
    stages: {
      latency: {
        status: "complete",
        result: {
          reportedMs: 12.4,
          minMs: 8.9,
          p50Ms: 12.4,
          p95Ms: 27.8,
          jitterMs: 2.2,
          packetLossPct: 0.4,
          method: "full-average",
          stabilityScore: 0.91,
          band: "high",
        },
        lanes: {
          latency: latencyLane,
          download: latencyLane,
          upload: { ...latencyLane, center: 18.2 },
          bidirectional: { ...latencyLane, center: 23.9 },
        },
      },
      download: { status: "complete", result: lane(download) },
      upload: { status: "complete", result: lane(61_500_000) },
      bidirectional: {
        status: "complete",
        down: lane(88_000_000),
        up: lane(42_000_000),
      },
    },
    bufferbloat: {
      idleMs: 12.4,
      loadedMs: 23.9,
      increaseMs: 11.5,
      grade: "A",
    },
    totalBytes: 39_999_999_996,
    server: {
      name: "Graphite Meter transcontinental validation edge with a deliberately long name",
      location: "Northern Europe · rack 127",
      engine: "dummy",
    },
    transport: {
      throughput: { protocol: "h3", kind: "webtransport" },
      latency: { protocol: "h3", kind: "webtransport" },
    },
    ipVersion: 6,
    client: { build: "0.0.0-browser-test+long-revision" },
    failures: [],
    wireEstimates: {
      version: 1,
      downloadBytesPerSec: download * 1.03,
      uploadBytesPerSec: 61_500_000 * 1.03,
      bidirectionalBytesPerSec: 130_000_000 * 1.03,
    },
  };
}

async function seedHistory(
  page: TestPage,
  values: unknown[],
  notify = true,
  generation?: unknown,
) {
  await page.evaluate(
    (input) =>
      new Promise<void>((resolve, reject) => {
        const opening = indexedDB.open(input.db.name, input.db.version);
        opening.onupgradeneeded = () => {
          const database = opening.result;
          const store = database.objectStoreNames.contains(
            input.db.resultsStore,
          )
            ? opening.transaction!.objectStore(input.db.resultsStore)
            : database.createObjectStore(input.db.resultsStore, {
                keyPath: input.db.resultKeyPath,
              });
          if (!store.indexNames.contains(input.db.completedAtIndex))
            store.createIndex(
              input.db.completedAtIndex,
              input.db.completedAtIndex,
            );
          if (!database.objectStoreNames.contains(input.db.metadataStore))
            database.createObjectStore(input.db.metadataStore, {
              keyPath: input.db.metadataKeyPath,
            });
        };
        opening.onerror = () => reject(opening.error);
        opening.onsuccess = () => {
          const db = opening.result;
          const transaction = db.transaction(
            input.generation === undefined
              ? input.db.resultsStore
              : [input.db.resultsStore, input.db.metadataStore],
            "readwrite",
          );
          const store = transaction.objectStore(input.db.resultsStore);
          store.clear();
          for (const item of input.items) store.put(item);
          if (input.generation !== undefined)
            transaction.objectStore(input.db.metadataStore).put({
              key: input.db.generationKey,
              value: input.generation,
            });
          transaction.oncomplete = () => {
            db.close();
            if (input.notify)
              window.dispatchEvent(new Event("graphite-meter-history-changed"));
            resolve();
          };
          transaction.onerror = () => reject(transaction.error);
        };
      }),
    { items: values, notify, generation, db: HISTORY_DB },
  );
}

async function rawHistoryState(page: TestPage) {
  return page.evaluate(
    (schema) =>
      new Promise<{ records: unknown[]; generation: unknown }>(
        (resolve, reject) => {
          const opening = indexedDB.open(schema.name, schema.version);
          opening.onerror = () => reject(opening.error);
          opening.onsuccess = () => {
            const database = opening.result;
            const transaction = database.transaction(
              [schema.resultsStore, schema.metadataStore],
              "readonly",
            );
            const records = transaction
              .objectStore(schema.resultsStore)
              .getAll();
            const generation = transaction
              .objectStore(schema.metadataStore)
              .get(schema.generationKey);
            transaction.onerror = () => reject(transaction.error);
            transaction.oncomplete = () => {
              database.close();
              resolve({
                records: records.result,
                generation: generation.result?.value,
              });
            };
          };
        },
      ),
    HISTORY_DB,
  );
}

async function setHistoryPreference(
  page: TestPage,
  preference: "enabled" | "disabled",
) {
  await page.evaluate((next) => {
    const stored = JSON.parse(
      localStorage.getItem("graphite-meter:v1") ?? "{}",
    );
    localStorage.setItem(
      "graphite-meter:v1",
      JSON.stringify({ ...stored, resultHistoryPreference: next }),
    );
    window.dispatchEvent(
      new StorageEvent("storage", { key: "graphite-meter:v1" }),
    );
  }, preference);
}

async function openHistory(page: TestPage, id: string | null = null) {
  await page.evaluate(
    (selected) =>
      (window.location.hash = selected ? `/history/${selected}` : "/history"),
    id,
  );
  await expect(page.getByRole("heading", { name: "History" })).toBeVisible();
}

async function toggleHistoryFromTopbar(page: TestPage) {
  const direct = page.getByRole("button", { name: /^(Open|Close) History$/ });
  if ((await direct.state()).some((state) => state.visible)) {
    await direct.click();
    return;
  }
  await page.getByRole("button", { name: "More controls" }).click();
  await page.getByRole("menuitem", { name: /^(Open|Close) History/ }).click();
}

test("the measurement route keeps History UI and IndexedDB lazy", async ({
  page,
}) => {
  await page.addInitScript(() => {
    const open = indexedDB.open.bind(indexedDB);
    (window as typeof window & { historyDbOpens: number }).historyDbOpens = 0;
    Object.defineProperty(indexedDB, "open", {
      configurable: true,
      value(name: string, version?: number) {
        (window as typeof window & { historyDbOpens: number }).historyDbOpens +=
          1;
        return version === undefined ? open(name) : open(name, version);
      },
    });
  });
  await openApp(page, "dummy");
  expect(
    await page.evaluate(
      () =>
        (window as typeof window & { historyDbOpens: number }).historyDbOpens,
    ),
  ).toBe(0);
  expect(
    await page.evaluate(() =>
      performance
        .getEntriesByType("resource")
        .some((entry) => entry.name.includes("HistoryWorkspace")),
    ),
  ).toBe(false);

  await openHistory(page);
  expect(
    await page.evaluate(
      () =>
        (window as typeof window & { historyDbOpens: number }).historyDbOpens,
    ),
  ).toBeGreaterThan(0);
  expect(
    await page.evaluate(() =>
      performance
        .getEntriesByType("resource")
        .some((entry) => entry.name.includes("HistoryWorkspace")),
    ),
  ).toBe(true);
});

test("explicitly enabled completion is reachable in History and stays responsive", async ({
  page,
}) => {
  await openApp(page, "dummy", { width: 390, height: 844 });
  const settings = await openSettings(page);
  await settings
    .getByText("Save completed results on this device", { exact: true })
    .click();
  await settings.getByText("Show estimated wire rate", { exact: true }).click();
  await expect(page.getByLabel("Show estimated wire rate")).not.toBeChecked();
  await settings.getByRole("button", { name: "short", exact: true }).click();
  await settings.getByRole("button", { name: "Close Settings" }).click();
  await startTest(page);
  await waitForCompletion(page, 20_000);
  await expect(
    page.locator('[data-latency-profile][data-variant="bare"]'),
  ).toBeVisible();
  await toggleHistoryFromTopbar(page);
  await expect(
    page.getByRole("button", { name: "Close History" }),
  ).toBeVisible();
  await expect(page.getByRole("button", { name: /^Theme:/ })).toBeVisible();
  await expect(
    page.getByRole("button", { name: "Toggle endpoint info" }),
  ).toBeVisible();
  await expect(page.getByRole("button", { name: "More controls" })).toHaveCount(
    0,
  );
  await expectNoHorizontalOverflow(page.locator(".topbar"));
  await expect(page.getByRole("heading", { name: "History" })).toBeVisible();
  await expect(page.locator(".overview-primary")).toContainText("1");
  const savedWireRate = await page.evaluate(
    (authority) =>
      new Promise<number | null>((resolve, reject) => {
        const opening = indexedDB.open(authority.name, authority.version);
        opening.onerror = () => reject(opening.error);
        opening.onsuccess = () => {
          const db = opening.result;
          const request = db
            .transaction(authority.resultsStore, "readonly")
            .objectStore(authority.resultsStore)
            .getAll();
          request.onerror = () => reject(request.error);
          request.onsuccess = () => {
            const rows = request.result as HistoryRecord[];
            db.close();
            resolve(rows[0]?.wireEstimates?.downloadBytesPerSec ?? null);
          };
        };
      }),
    HISTORY_DB,
  );
  expect(savedWireRate).not.toBeNull();
  expect(savedWireRate!).toBeGreaterThan(0);
  await page.locator("a.result-row").first().click();
  await expect(
    page.getByRole("heading", { name: "Wire-rate snapshot" }),
  ).toHaveCount(0);
  const displaySettings = await openSettings(page);
  await displaySettings
    .getByText("Show estimated wire rate", { exact: true })
    .click();
  await displaySettings.getByRole("button", { name: "Close Settings" }).click();
  await expect(
    page.getByRole("heading", { name: "Wire-rate snapshot" }),
  ).toBeVisible();
  await expectNoHorizontalOverflow(page.locator(".history-workspace"));
  await expectNoHorizontalOverflow(page.locator("body"));
});

test("topbar controls compact progressively instead of hiding Theme on phones", async ({
  page,
}) => {
  await openApp(page, "dummy", { width: 390, height: 844 });
  await setHistoryPreference(page, "enabled");
  await expect(page.getByRole("button", { name: /^Theme:/ })).toBeVisible();
  await expect(
    page.getByRole("button", { name: "Open History" }),
  ).toBeVisible();
  await expect(
    page.getByRole("button", { name: "Toggle endpoint info" }),
  ).toBeVisible();
  await expect(page.getByRole("button", { name: "More controls" })).toHaveCount(
    0,
  );
  await expectNoHorizontalOverflow(page.locator(".topbar"));

  await page.setViewportSize({ width: 319, height: 844 });
  await page.getByRole("button", { name: "More controls" }).click();
  await expect(
    page.getByRole("menuitem", { name: /Open History/ }),
  ).toBeVisible();
  await expect(
    page.getByRole("menuitem", { name: /Endpoint info/ }),
  ).toBeVisible();
  await expect(page.getByRole("menuitem", { name: /Theme:/ })).toBeVisible();
  await page.keyboard.press("Escape");
  await expectNoHorizontalOverflow(page.locator(".topbar"));

  await page.setViewportSize({ width: 700, height: 844 });
  await expect(
    page.getByRole("button", { name: "Open History" }),
  ).toBeVisible();
  await expect(page.getByRole("button", { name: /^Theme:/ })).toBeVisible();
  await expect(
    page.getByRole("button", { name: "Toggle endpoint info" }),
  ).toBeVisible();
  await expect(page.getByRole("button", { name: "More controls" })).toHaveCount(
    0,
  );

  await page.setViewportSize({ width: 844, height: 390 });
  await expect(
    page.getByRole("button", { name: "Open History" }),
  ).toBeVisible();
  await expect(
    page.getByRole("button", { name: "Toggle endpoint info" }),
  ).toBeVisible();
  await expect(page.getByRole("button", { name: "More controls" })).toHaveCount(
    0,
  );
  await expectNoHorizontalOverflow(page.locator(".topbar"));
});

test("History is safe to reload and malformed client routes stay in the shell", async ({
  page,
}) => {
  await openApp(page, "dummy");
  await openHistory(page);
  await page.reload();
  await expect(page.getByRole("heading", { name: "History" })).toBeVisible();
  await seedHistory(
    page,
    [record(IDS.newest, Date.UTC(2026, 7, 28, 12))],
    false,
  );
  await page.evaluate(() => window.dispatchEvent(new Event("focus")));
  await expect(page.locator(".result-row")).toHaveCount(1);
  await page.evaluate(() => (window.location.hash = "/unknown"));
  await expect(
    page.getByRole("heading", { name: "Page not found" }),
  ).toBeVisible();
});

test("an out-of-window deep link preserves the 2,000-summary memory cap", async ({
  page,
}) => {
  await openApp(page, "dummy", { width: 1366, height: 768 });
  const base = Date.UTC(2026, 7, 28, 12);
  const values = Array.from({ length: 2_001 }, (_, index) =>
    record(
      `00000000-0000-4000-8000-${index.toString(16).padStart(12, "0")}`,
      base - index * 60_000,
    ),
  );
  const oldest = values.at(-1)!;
  await seedHistory(page, values);
  await openHistory(page, oldest.id);

  await expect(page.locator(".result-detail")).toBeVisible();
  await expect(page.locator(".overview-primary")).toContainText(
    "2000 results saved locally",
  );
});

test("sorting a 2,000-result archive keeps the visible chunk bounded", async ({
  page,
}) => {
  await openApp(page, "dummy", { width: 1366, height: 768 });
  const base = Date.UTC(2026, 7, 28, 12);
  const values = Array.from({ length: 2_000 }, (_, index) =>
    record(
      `00000000-0000-4000-8000-${index.toString(16).padStart(12, "0")}`,
      base - index * 60_000,
      1_000_000 + index,
    ),
  );
  await seedHistory(page, values);
  await openHistory(page);
  await expect(page.locator(".result-row")).toHaveCount(50);

  const profile = await page.evaluate(
    (db) =>
      new Promise<{ nestedMs: number; cachedMs: number }>((resolve, reject) => {
        const opening = indexedDB.open(db.name, db.version);
        opening.onerror = () => reject(opening.error);
        opening.onsuccess = () => {
          const database = opening.result;
          const request = database
            .transaction(db.resultsStore, "readonly")
            .objectStore(db.resultsStore)
            .getAll();
          request.onerror = () => reject(request.error);
          request.onsuccess = () => {
            const records = request.result;
            database.close();
            const stable = (a: any, b: any) =>
              b.completedAt - a.completedAt ||
              String(b.id).localeCompare(String(a.id));
            const nestedSort = () =>
              [...records].sort((a, b) => {
                const av =
                  a.stages.download.result?.reportedBytesPerSec ?? null;
                const bv =
                  b.stages.download.result?.reportedBytesPerSec ?? null;
                if (av == null && bv == null) return stable(a, b);
                if (av == null) return 1;
                if (bv == null) return -1;
                return av === bv ? stable(a, b) : bv - av;
              });
            const prepared = records.map((record) => ({
              record,
              key: record.stages.download.result?.reportedBytesPerSec ?? null,
              id: record.id,
              completedAt: record.completedAt,
            }));
            const cachedSort = () =>
              [...prepared].sort((a, b) => {
                if (a.key == null && b.key == null) return stable(a, b);
                if (a.key == null) return 1;
                if (b.key == null) return -1;
                return a.key === b.key ? stable(a, b) : b.key - a.key;
              });
            nestedSort();
            cachedSort();
            const iterations = 40;
            let started = performance.now();
            for (let index = 0; index < iterations; index++) nestedSort();
            const nestedMs = performance.now() - started;
            started = performance.now();
            for (let index = 0; index < iterations; index++) cachedSort();
            resolve({
              nestedMs,
              cachedMs: performance.now() - started,
            });
          };
        };
      }),
    HISTORY_DB,
  );
  const elapsed = await page.evaluate(
    (expectedId) =>
      new Promise<number>((resolve, reject) => {
        const button = document.querySelector<HTMLButtonElement>(
          '.column-head [data-tone="download"] button',
        );
        const list = document.querySelector(".archive-list ol");
        if (!button || !list) {
          reject(new Error("History sort controls are unavailable"));
          return;
        }
        const started = performance.now();
        const finish = () => {
          if (
            document
              .querySelector(".result-row")
              ?.getAttribute("data-history-id") !== expectedId
          )
            return false;
          observer.disconnect();
          window.clearTimeout(timeout);
          resolve(performance.now() - started);
          return true;
        };
        const observer = new MutationObserver(finish);
        const timeout = window.setTimeout(() => {
          observer.disconnect();
          reject(new Error("History sort did not render"));
        }, 2_000);
        observer.observe(list, { childList: true, subtree: true });
        button.click();
        queueMicrotask(finish);
      }),
    values.at(-1)!.id,
  );
  await expect(page.locator(".result-row").first()).toHaveAttribute(
    "data-history-id",
    values.at(-1)!.id,
  );
  console.info(
    `[history-sort-profile] 2000 rows × 40: nested ${profile.nestedMs.toFixed(2)} ms, cached ${profile.cachedMs.toFixed(2)} ms; interaction ${elapsed.toFixed(2)} ms`,
  );
  expect(profile.nestedMs).toBeGreaterThan(0);
  expect(profile.cachedMs).toBeGreaterThan(0);
  expect(Number.isFinite(elapsed)).toBe(true);
  await expect(page.locator(".result-row")).toHaveCount(50);

  for (const width of [390, 340, 320]) {
    await page.setViewportSize({ width, height: 844 });
    const heights = await page
      .locator(".result-row")
      .evaluateAll((rows) =>
        rows.slice(0, 10).map((row) => row.getBoundingClientRect().height),
      );
    expect(Math.max(...heights)).toBeLessThanOrEqual(80);
    await expectNoHorizontalOverflow(page.locator(".history-workspace"));
  }
  await page.setViewportSize({ width: 1366, height: 768 });

  const listScroll = await page
    .locator(".archive-list")
    .evaluate((node) => node.scrollTop);
  expect(listScroll).toBe(0);
  const management = page.getByRole("button", { name: "Archive management" });
  await expect(management).toBeVisible();
  await management.click();
  await page.getByRole("menuitem", { name: /Clear all saved results/ }).click();
  const clearDialog = page.getByRole("alertdialog", {
    name: "Clear result history?",
  });
  await clearDialog.getByRole("button", { name: "Cancel" }).click();
  await expect(management).toBeFocused();
  await management.click();
  await page.getByRole("menuitem", { name: /Clear all saved results/ }).click();
  await clearDialog.getByRole("button", { name: "Clear history" }).click();
  await expect(page.getByText("No saved results")).toBeVisible();
});

test("malformed-only archives keep a raw clear path", async ({ page }) => {
  await openApp(page, "dummy", { width: 900, height: 700 });
  await seedHistory(page, [record(IDS.oldest, 1e20)]);
  await openHistory(page);
  await expect(page.getByText(/1 malformed record was ignored/)).toBeVisible();
  const management = page.getByRole("button", { name: "Archive management" });
  await expect(management).toBeVisible();
  await management.click();
  await page.getByRole("menuitem", { name: /Clear all saved results/ }).click();
  const dialog = page.getByRole("alertdialog", {
    name: "Clear result history?",
  });
  await expect(
    dialog.getByText(
      "Permanently remove all locally stored results from this browser?",
    ),
  ).toBeVisible();
  await dialog.getByRole("button", { name: "Clear history" }).click();
  await expect(page.getByText("No saved results")).toBeVisible();
  const count = await page.evaluate(
    (db) =>
      new Promise<number>((resolve, reject) => {
        const request = indexedDB.open(db.name, db.version);
        request.onerror = () => reject(request.error);
        request.onsuccess = () => {
          const tx = request.result.transaction(db.resultsStore, "readonly");
          const get = tx.objectStore(db.resultsStore).count();
          get.onsuccess = () => resolve(get.result);
        };
      }),
    HISTORY_DB,
  );
  expect(count).toBe(0);
});

test("repository repairs corrupt generation metadata without deleting raw rows", async ({
  page,
}) => {
  await openApp(page, "dummy", { width: 1024, height: 768 });
  await setHistoryPreference(page, "enabled");
  const settings = await openSettings(page);
  await settings.getByRole("button", { name: "short", exact: true }).click();
  await settings.getByRole("button", { name: "Close Settings" }).click();

  const existing = record(IDS.oldest, Date.UTC(2026, 7, 28, 12));
  const malformed = {
    id: IDS.middle,
    completedAt: Date.UTC(2026, 7, 28, 11),
    unexpected: "raw malformed row",
  };
  await seedHistory(page, [existing, malformed], false, { corrupt: true });
  await page.evaluate(() => {
    localStorage.removeItem("graphite-meter:history-generation");
    (
      window as typeof window & { historySaveEvents: number }
    ).historySaveEvents = 0;
    window.addEventListener("graphite-meter-history-changed", () => {
      (
        window as typeof window & { historySaveEvents: number }
      ).historySaveEvents += 1;
    });
  });

  await startTest(page);
  await waitForCompletion(page, 20_000);
  await expect
    .poll(async () => (await rawHistoryState(page)).records.length)
    .toBe(3);
  const state = await rawHistoryState(page);
  expect(isHistoryGeneration(state.generation)).toBe(true);
  expect(isRepairHistoryGeneration(state.generation as string)).toBe(true);
  expect(
    state.records.some(
      (candidate) =>
        typeof candidate === "object" &&
        candidate !== null &&
        (candidate as { id?: string }).id === existing.id,
    ),
  ).toBe(true);
  expect(
    state.records.some(
      (candidate) =>
        typeof candidate === "object" &&
        candidate !== null &&
        (candidate as { unexpected?: string }).unexpected ===
          "raw malformed row",
    ),
  ).toBe(true);
  expect(
    await page.evaluate(
      () =>
        (window as typeof window & { historySaveEvents: number })
          .historySaveEvents,
    ),
  ).toBe(1);

  await openHistory(page);
  await expect(page.locator(".result-row")).toHaveCount(2);
  await expect(page.getByText(/1 malformed record was ignored/)).toBeVisible();
});

test("wordmark semantics preserve a live run away from the meter", async ({
  page,
}) => {
  await openApp(page, "dummy", { width: 1024, height: 768 });
  await startTest(page);
  await expect(page.getByRole("button", { name: "Abort test" })).toBeVisible();

  await page
    .getByRole("button", {
      name: "Graphite Meter — return to a fresh, blank test",
    })
    .click();
  const confirm = page.getByRole("alertdialog", {
    name: "Stop the running test?",
  });
  await expect(confirm).toBeVisible();
  await confirm.getByRole("button", { name: "Keep running" }).click();

  await openHistory(page);
  const indicator = page.locator(".return-live");
  await expect(indicator).toBeVisible();
  await expect(indicator).toHaveAttribute(
    "aria-label",
    /Return to live meter\.$/,
  );
  expect(
    await indicator
      .locator(".live-copy")
      .evaluate((node) => getComputedStyle(node).columnGap),
  ).toBe("4px");
  await page.keyboard.press("Escape");
  await expect(page.getByRole("button", { name: "Abort test" })).toBeVisible();
  await openHistory(page);
  await expect(indicator).toBeVisible();
  await page.setViewportSize({ width: 390, height: 844 });
  await expect(indicator.locator(".live-phase")).toBeHidden();
  await expectNoHorizontalOverflow(page.locator(".topbar"));
  await page
    .getByRole("button", { name: "Graphite Meter — return to live meter" })
    .click();
  await expect(page.getByRole("button", { name: "Abort test" })).toBeVisible();
});

test("wide panels compose over History while narrow routes keep only the visible panel", async ({
  page,
}) => {
  await openApp(page, "dummy", { width: 1440, height: 900 });
  await seedHistory(page, [record(IDS.newest, Date.UTC(2026, 7, 28, 12))]);
  const settings = await openSettings(page);
  await settings.getByRole("link", { name: "View History" }).click();
  await expect(page.getByRole("heading", { name: "History" })).toBeVisible();
  await expect(settingsPanel(page)).toBeVisible();

  const endpoint = await openEndpointInfo(page);
  await expect(settingsPanel(page)).toBeVisible();
  await expect(endpoint).toBeVisible();
  expect(await page.evaluate(() => window.location.hash)).toContain(
    "panels=settings,endpoint",
  );

  await endpoint.getByRole("button", { name: "About & legal" }).click();
  await expect(
    page.getByRole("dialog", { name: "About & legal" }),
  ).toBeVisible();
  await expect(page.getByRole("heading", { name: "History" })).toBeVisible();
  await page.keyboard.press("h");
  await expect(
    page.getByRole("dialog", { name: "About & legal" }),
  ).toBeVisible();
  expect(await page.evaluate(() => window.location.hash)).toContain(
    "panels=settings,endpoint",
  );
  expect(await page.evaluate(() => window.location.hash)).toContain(
    "dialog=legal",
  );
  await expect(page.getByRole("heading", { name: "History" })).toHaveCount(0);
  await page.keyboard.press("h");
  await expect(page.getByRole("heading", { name: "History" })).toBeVisible();
  await expect(
    page.getByRole("dialog", { name: "About & legal" }),
  ).toBeVisible();
  await page.keyboard.press("Escape");

  await page.setViewportSize({ width: 390, height: 844 });
  await expect(settingsPanel(page)).toHaveAttribute("inert", "");
  await expect(endpointPanel(page)).toBeVisible();
  await expect
    .poll(() => page.evaluate(() => window.location.hash))
    .toBe("#/history?panels=endpoint");

  await endpoint.getByRole("button", { name: "Close Endpoint" }).click();
  expect(await page.evaluate(() => window.location.hash)).toBe("#/history");
  await expect(settingsPanel(page)).toHaveAttribute("inert", "");

  await openSettings(page);
  expect(await page.evaluate(() => window.location.hash)).toBe(
    "#/history?panels=settings",
  );
  const switchedEndpoint = await openEndpointInfo(page);
  expect(await page.evaluate(() => window.location.hash)).toBe(
    "#/history?panels=endpoint",
  );
  await switchedEndpoint
    .getByRole("button", { name: "Close Endpoint" })
    .click();
  expect(await page.evaluate(() => window.location.hash)).toBe("#/history");
  await expect(settingsPanel(page)).toHaveAttribute("inert", "");

  await page.setViewportSize({ width: 1440, height: 900 });
  await expect(settingsPanel(page)).toHaveAttribute("inert", "");
  await expect(endpointPanel(page)).toHaveAttribute("inert", "");
});

test("deep-linked detail uses contextual focus without outlining its heading", async ({
  page,
}) => {
  await openApp(page, "dummy", { width: 1440, height: 900 });
  await seedHistory(page, [record(IDS.newest, Date.UTC(2026, 7, 28, 12))]);
  await openHistory(page, IDS.newest);
  const heading = page.locator(".result-detail h2");
  const detailRegion = page.locator(".result-detail");
  await expect(page.locator(".detail-inspector")).toBeVisible();
  await expect(detailRegion).toBeFocused();
  await expect(heading).not.toBeFocused();
  expect(await heading.getAttribute("tabindex")).toBeNull();
  expect(
    await detailRegion.evaluate((node) => getComputedStyle(node).outlineStyle),
  ).toBe("none");
  const detail = page.locator(".detail-inspector");
  const profile = detail.locator(
    '[data-latency-profile][data-variant="compact"]',
  );
  await expect(profile).toBeVisible();
  await expect(profile.locator(".lane")).toHaveCount(4);
  await expect(detail.getByText("Median (p50)", { exact: true })).toBeVisible();
  await expect(detail.getByText("p95", { exact: true })).toBeVisible();
  await expect(detail.getByText("Stability", { exact: true })).toBeVisible();
  await expect(detail.locator("details")).toHaveCount(0);
  await expect(detail.locator(".throughput-card")).toHaveCount(3);
  await expect(detail.locator(".throughput-card em")).toHaveCount(0);
  await expect(detail.locator(".throughput-card")).not.toContainText(/loss/i);
  await expect(
    detail.getByRole("heading", { name: "Run context" }),
  ).toBeVisible();
  await expect(
    detail.getByRole("heading", { name: "Stage issues" }),
  ).toHaveCount(0);
  await expect(detail.getByText(/grade|increase/i)).toHaveCount(0);
  const firstTrack = profile.locator(".track").first();
  await firstTrack.focus();
  await expect(profile.locator(".hover-card")).toBeVisible();
  await expect(profile.locator(".hover-card")).not.toContainText(/loss/i);
  await firstTrack.press("ArrowRight");
  await expect(profile.locator(".hover-card")).toContainText(/P10|Result|P90/);
  await profile.locator(".track").nth(1).focus();
  await expect(profile.locator(".hover-card")).toContainText(/Avg/);
  await expect(
    detail.getByRole("button", { name: "Close result" }),
  ).toBeVisible();
  await expectNoHorizontalOverflow(page.locator(".history-workspace"));

  await page.keyboard.press("Escape");
  const row = page.locator(`[data-history-id="${IDS.newest}"]`);
  await expect(row).toBeFocused();
  expect(await page.evaluate(() => window.location.hash)).toBe("#/history");

  await page.setViewportSize({ width: 390, height: 844 });
  await row.click();
  await expect(page.locator(".inline-inspector")).toBeVisible();
  await expect(row).toBeFocused();
  await expect(heading).not.toBeFocused();
  await expect(row.getByText("Selected", { exact: true })).toBeVisible();
  await expect(page.locator(".detail-inspector")).toHaveCount(0);
  await expectNoHorizontalOverflow(page.locator(".history-workspace"));

  const accessibility = await new AxeBuilder({ page })
    .include(".history-workspace")
    .analyze();
  expect(accessibility.violations).toEqual([]);
  await page.evaluate(() =>
    document.documentElement.setAttribute("data-theme", "dark"),
  );
  const darkAccessibility = await new AxeBuilder({ page })
    .include(".history-workspace")
    .analyze();
  expect(darkAccessibility.violations).toEqual([]);
});

test("detail, Legal, and History keyboard routes keep focus on meaningful targets", async ({
  page,
}) => {
  await openApp(page, "dummy", { width: 1024, height: 768 });
  await setHistoryPreference(page, "enabled");
  await seedHistory(page, [record(IDS.newest, Date.UTC(2026, 7, 28, 12))]);
  await openHistory(page);

  const row = page.locator(`[data-history-id="${IDS.newest}"]`);
  const heading = page.locator(".result-detail h2");
  const closeResult = page.getByRole("button", { name: "Close result" });
  await expect(row).toBeVisible();
  await row.focus();
  await row.press("Enter");
  await expect(closeResult).toBeFocused();
  await expect(heading).not.toBeFocused();
  expect(await heading.getAttribute("tabindex")).toBeNull();

  await page.evaluate(() => window.history.back());
  await expect(row).toBeFocused();
  await row.press("Enter");
  await expect(closeResult).toBeFocused();
  await page.keyboard.press("Escape");
  await expect(row).toBeFocused();
  await row.press("Enter");
  await expect(closeResult).toBeFocused();

  const endpoint = await openEndpointInfo(page);
  const legalInvoker = endpoint.getByRole("button", { name: "About & legal" });
  await legalInvoker.click();
  await expect(
    page.getByRole("dialog", { name: "About & legal" }),
  ).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(legalInvoker).toBeFocused();
  await expect(heading).not.toBeFocused();
  await endpoint.getByRole("button", { name: "Close Endpoint" }).click();
  await expect(
    page.getByRole("button", { name: "Toggle endpoint info" }),
  ).toBeFocused();

  await page.keyboard.press("h");
  await expect(page.locator(".measurement-stage")).toBeFocused();
  await expect(page.locator(".brand-btn")).not.toBeFocused();
  await page.keyboard.press("h");
  await expect(page.locator(".history-workspace")).toBeFocused();
  await expect(page.locator(".result-detail h2")).toHaveCount(0);
  await expect(page.getByRole("tooltip")).toHaveCount(0);
});

test("activating the selected result closes detail without hijacking modified links", async ({
  page,
}) => {
  await openApp(page, "dummy", { width: 1440, height: 900 });
  await seedHistory(page, [record(IDS.newest, Date.UTC(2026, 7, 28, 12))]);
  await openHistory(page);
  const settings = await openSettings(page);
  const row = page.locator(`[data-history-id="${IDS.newest}"]`);
  const detail = page.locator(".result-detail");

  await row.click();
  await expect(detail).toBeVisible();
  await expect(row).toHaveAttribute("aria-current", "true");
  await expect(row).toHaveAttribute("aria-expanded", "true");
  expect(await page.evaluate(() => window.location.hash)).toBe(
    `#/history/${IDS.newest}?panels=settings`,
  );

  for (const eventInit of [
    { ctrlKey: true },
    { metaKey: true },
    { shiftKey: true },
    { button: 1 },
  ]) {
    const wasPrevented = await row.evaluate((node, init) => {
      let prevented: boolean | null = null;
      document.addEventListener(
        "click",
        (event) => {
          prevented = event.defaultPrevented;
          event.preventDefault();
        },
        { once: true },
      );
      node.dispatchEvent(
        new MouseEvent("click", {
          bubbles: true,
          cancelable: true,
          ...init,
        }),
      );
      return prevented;
    }, eventInit);
    expect(wasPrevented).toBe(false);
    await expect(detail).toBeVisible();
  }

  await row.click();
  await expect(detail).toHaveCount(0);
  await expect(row).toBeFocused();
  expect(await row.getAttribute("aria-current")).toBeNull();
  await expect(row).toHaveAttribute("aria-expanded", "false");
  expect(await page.evaluate(() => window.location.hash)).toBe(
    "#/history?panels=settings",
  );
  await expect(settings).toBeVisible();

  await row.press("Enter");
  await expect(detail).toBeVisible();
  await row.focus();
  await row.press("Enter");
  await expect(detail).toHaveCount(0);
  await expect(row).toBeFocused();
  expect(await page.evaluate(() => window.location.hash)).toBe(
    "#/history?panels=settings",
  );

  await settings.getByRole("button", { name: "Close Settings" }).click();
  await page.setViewportSize({ width: 390, height: 844 });
  await row.click();
  await expect(page.locator(".inline-inspector")).toBeVisible();
  await row.click();
  await expect(page.locator(".inline-inspector")).toHaveCount(0);
  await expect(row).toBeFocused();
  expect(await page.evaluate(() => window.location.hash)).toBe("#/history");

  await page.setViewportSize({ width: 768, height: 1024 });
  await row.press("Enter");
  await expect(detail).toBeVisible();
  await row.focus();
  await row.press("Enter");
  await expect(detail).toHaveCount(0);
  await expect(row).toBeFocused();
});

test("saved probe timeouts identify their transport and preserve legacy results", async ({
  page,
}) => {
  await openApp(page, "dummy", { width: 1366, height: 768 });
  const webtransport = record(IDS.newest, Date.UTC(2026, 7, 28, 12));
  webtransport.stages.latency.lanes.download = null;
  webtransport.stages.latency.lanes.upload = {
    ...webtransport.stages.latency.lanes.upload!,
    lossRatio: 0,
  };
  webtransport.stages.latency.lanes.bidirectional = null;
  const websocket = record(IDS.middle, Date.UTC(2026, 7, 27, 12));
  websocket.transport.latency = { protocol: "h2", kind: "websocket" };
  const unknown = record(IDS.oldest, Date.UTC(2026, 7, 26, 12));
  unknown.transport.latency = { protocol: null, kind: null };
  await seedHistory(page, [webtransport, websocket, unknown]);
  await openHistory(page, webtransport.id);
  const profile = page.locator(
    '[data-latency-profile][data-variant="compact"]',
  );
  await expect(profile).toHaveAttribute(
    "aria-label",
    "Saved latency distributions",
  );
  const firstTrack = profile.locator(".track").first();
  await expect(firstTrack).not.toHaveAttribute("aria-label", /loss/);
  await expect(profile.locator(".loss-marker")).toHaveCount(0);
  await firstTrack.focus();
  await expect(profile.locator(".hover-card")).toBeVisible();
  await expect(profile.locator(".hover-card")).not.toContainText(/loss/i);
  const probeTimeouts = page.locator(".probe-timeouts-section");
  await expect(probeTimeouts).toBeVisible();
  await expect(
    probeTimeouts.getByRole("heading", { name: "Probe timeouts (datagram)" }),
  ).toBeVisible();
  const lossHelp = probeTimeouts.getByRole("note", {
    name: "About probe timeouts",
  });
  await expect(lossHelp).toHaveAttribute("tabindex", "0");
  await lossHelp.hover();
  await expect(page.getByRole("tooltip")).toHaveText(
    "Application probes whose reply deadline expired. WebTransport uses datagrams; WebSocket uses a reliable stream. Neither identifies physical or directional IP packet loss. Interrupted and locally rejected sends are excluded.",
  );
  await page.mouse.move(0, 0);
  await expect(page.getByRole("tooltip")).toHaveCount(0);
  await expect(probeTimeouts.locator(".probe-timeouts-lanes li")).toHaveCount(
    2,
  );
  await expect(
    probeTimeouts.locator('.probe-timeouts-lanes li[data-tone="latency"]'),
  ).toHaveAttribute("aria-label", "Idle probe timeouts 1%, 140 resolved");
  await expect(
    probeTimeouts.locator('.probe-timeouts-lanes li[data-tone="upload"]'),
  ).toHaveAttribute("aria-label", "Loaded Up probe timeouts 0%, 140 resolved");
  await expect(
    probeTimeouts.locator('.probe-timeouts-lanes li[data-tone="upload"] em'),
  ).toHaveText("0%");
  await expect(probeTimeouts).not.toContainText("Loaded Down");
  await expect(probeTimeouts).not.toContainText("Loaded Bi-dir");

  await page.evaluate(
    (id) => (window.location.hash = `/history/${id}`),
    websocket.id,
  );
  await expect(
    probeTimeouts.getByRole("heading", { name: "Probe timeouts (WebSocket)" }),
  ).toBeVisible();
  await expect(profile.locator(".loss-marker")).toHaveCount(0);
  await expect(profile.locator(".track").first()).not.toHaveAttribute(
    "aria-label",
    /loss/,
  );

  await page.evaluate(
    (id) => (window.location.hash = `/history/${id}`),
    unknown.id,
  );
  await expect(probeTimeouts).toHaveCount(0);
  await expect(profile.locator(".loss-marker")).toHaveCount(0);
  await expect(profile.locator(".track").first()).not.toHaveAttribute(
    "aria-label",
    /loss/,
  );
});

test("recent rows use relative time and absent responsiveness stays concise", async ({
  page,
}) => {
  await openApp(page, "dummy", { width: 1366, height: 768 });
  const now = Date.now();
  const recent = record(IDS.newest, now - 3 * 60_000);
  recent.stages.latency = {
    status: "not-run",
    result: null,
    lanes: {
      latency: null,
      download: null,
      upload: null,
      bidirectional: null,
    },
  };
  recent.bufferbloat = null;
  const atBoundary = record(IDS.middle, now - 60 * 60_000);
  const underBoundary = record(IDS.oldest, now - (59 * 60_000 + 59_000));
  await seedHistory(page, [atBoundary, underBoundary, recent]);
  await openHistory(page);

  const recentRow = page.locator(`[data-history-id="${IDS.newest}"]`);
  await expect(recentRow.locator(".date-cell strong")).toHaveText("3 min ago");
  await expect(recentRow.locator(".date-cell strong")).toHaveAttribute(
    "title",
    /.+/,
  );
  await expect(
    recentRow.locator('.metric-cell[data-tone="loaded"]'),
  ).toContainText("Not run");
  await expect(
    page
      .locator(`[data-history-id="${IDS.middle}"]`)
      .locator(".date-cell strong"),
  ).not.toContainText("ago");
  await expect(
    page
      .locator(`[data-history-id="${IDS.oldest}"]`)
      .locator(".date-cell strong"),
  ).toHaveText("59 min ago");

  await recentRow.click();
  await expect(
    page.getByRole("heading", { name: "Responsiveness" }),
  ).toHaveCount(0);
  await expect(page.locator(".stage-unavailable")).toHaveCount(0);
});

test("History is a toggleable archive layer with an explicit close and no empty inspector", async ({
  page,
}) => {
  await openApp(page, "dummy", { width: 1366, height: 768 });
  await setHistoryPreference(page, "enabled");
  await seedHistory(page, [record(IDS.newest, Date.UTC(2026, 7, 28, 12))]);
  const settingsBox = await page
    .getByRole("button", { name: "Open settings" })
    .boundingBox();
  const historyBox = await page
    .getByRole("button", { name: "Open History" })
    .boundingBox();
  expect(settingsBox!.x).toBeLessThan(historyBox!.x);
  await toggleHistoryFromTopbar(page);
  await expect(page.locator(".detail-inspector")).toHaveCount(0);
  await expect(page.locator(".workspace-body.with-side")).toHaveCount(0);
  await expect(page.locator(".archive-overview")).not.toContainText("Saving");
  await expect(page.locator(".archive-overview")).not.toContainText("View");
  await expect(page.locator(".archive-toolbar")).not.toContainText("Clear");
  const management = page.getByRole("button", { name: "Archive management" });
  await management.click();
  await page.getByRole("menuitem", { name: /Clear all saved results/ }).click();
  const clearDialog = page.getByRole("alertdialog", {
    name: "Clear result history?",
  });
  await expect(clearDialog).toBeVisible();
  await clearDialog.getByRole("button", { name: "Cancel" }).click();
  await expect(management).toBeFocused();
  await expect(page.locator(".result-row")).toHaveCount(1);

  await setHistoryPreference(page, "disabled");
  await expect(page.locator(".saving-notice")).toBeVisible();
  await expect(page.locator(".result-row")).toHaveCount(1);
  await page.getByRole("button", { name: "Enable future saves" }).click();
  await expect(page.locator(".saving-notice")).toBeHidden();
  await toggleHistoryFromTopbar(page);
  await expect(
    page.getByRole("button", { name: "Start the speed test" }),
  ).toBeVisible();

  await openHistory(page);
  await page.locator(".close-history").click();
  await expect(
    page.getByRole("button", { name: "Start the speed test" }),
  ).toBeVisible();
  await expect(page.locator(".measurement-stage")).toBeFocused();
});

test("History shortcut uses neutral workspace focus and preserves its invocation context", async ({
  page,
}) => {
  await openApp(page, "dummy", { width: 1024, height: 768 });
  await setHistoryPreference(page, "enabled");
  await seedHistory(page, [record(IDS.newest, Date.UTC(2026, 7, 28, 12))]);

  const hint = page
    .locator(".command-hints span")
    .filter({ hasText: "History" });
  await expect(hint).toBeVisible();
  const settingsTrigger = page.getByRole("button", { name: "Open settings" });
  await settingsTrigger.focus();
  await page.keyboard.press("h");
  await expect(page.locator(".history-workspace")).toBeFocused();
  await expect(settingsTrigger).not.toBeFocused();
  await expect(page.locator(".brand-btn")).not.toBeFocused();
  await page.waitForTimeout(400);
  await expect(page.getByRole("tooltip")).toHaveCount(0);

  await page.keyboard.press("h");
  await expect(page.locator(".measurement-stage")).toBeFocused();
  await expect(page.locator(".brand-btn")).not.toBeFocused();
  await expect(settingsTrigger).not.toBeFocused();
  await expect(page.getByRole("tooltip")).toHaveCount(0);

  const endpointTrigger = page.getByRole("button", {
    name: "Toggle endpoint info",
  });
  await endpointTrigger.focus();
  await page.keyboard.press("h");
  await expect(page.locator(".history-workspace")).toBeFocused();
  await page.evaluate(() => window.history.back());
  await expect(page.locator(".measurement-stage")).toBeFocused();
  await expect(page.locator(".brand-btn")).not.toBeFocused();

  await page.getByRole("button", { name: "Open History" }).click();
  await expect(
    page.locator('.topbar [aria-label="Close History"]'),
  ).toBeFocused();
  await page.keyboard.press("Escape");
  await expect(
    page.getByRole("button", { name: "Open History" }),
  ).toBeFocused();

  await settingsTrigger.click();
  await expect(settingsPanel(page)).toBeVisible();
  await settingsPanel(page)
    .getByRole("button", { name: "Close Settings" })
    .click();
  await expect(settingsTrigger).toBeFocused();

  await page.keyboard.press("s");
  await expect(settingsPanel(page)).toBeVisible();
  await page.keyboard.press("s");
  await expect(page.locator(".measurement-stage")).toBeFocused();
  await page.keyboard.press("d");
  await expect(endpointPanel(page)).toBeVisible();
  await page.keyboard.press("d");
  await expect(page.locator(".measurement-stage")).toBeFocused();
});

test("History shortcut stays functional while saving is paused and ignores editable or modified input", async ({
  page,
}) => {
  await openApp(page, "dummy", { width: 1024, height: 768 });
  await setHistoryPreference(page, "disabled");
  await expect(
    page.locator(".command-hints span").filter({ hasText: "History" }),
  ).toHaveCount(0);

  const settings = await openSettings(page);
  const input = settings.locator("input").first();
  await input.focus();
  const settingsHash = await page.evaluate(() => window.location.hash);
  await page.keyboard.press("h");
  expect(await page.evaluate(() => window.location.hash)).toBe(settingsHash);
  await page.evaluate(() => {
    window.dispatchEvent(
      new KeyboardEvent("keydown", {
        key: "h",
        ctrlKey: true,
        bubbles: true,
      }),
    );
  });
  expect(await page.evaluate(() => window.location.hash)).toBe(settingsHash);

  await settings.getByRole("button", { name: "Close Settings" }).click();
  await page.keyboard.press("h");
  await expect(page.getByRole("heading", { name: "History" })).toBeVisible();
  await page.keyboard.press("h");
  await expect(page.locator(".measurement-stage")).toBeFocused();

  await startTest(page);
  await expect(page.getByRole("button", { name: "Abort test" })).toBeVisible();
  await page.keyboard.press("h");
  await expect(page.locator(".return-live")).toBeVisible();
  await page.keyboard.press("h");
  await expect(page.getByRole("button", { name: "Abort test" })).toBeVisible();
});

test("phone topbar keeps History, Theme, and Endpoint direct without a special active fill", async ({
  page,
}) => {
  await openApp(page, "dummy", { width: 390, height: 640 });
  await setHistoryPreference(page, "enabled");
  await seedHistory(page, [record(IDS.newest, Date.UTC(2026, 7, 28, 12))]);
  await startTest(page);
  await page.getByRole("button", { name: "Open History" }).click();

  const history = page.getByRole("button", { name: "Close History" });
  const theme = page.getByRole("button", { name: /^Theme:/ });
  const endpoint = page.getByRole("button", { name: "Toggle endpoint info" });
  await expect(history).toBeVisible();
  await expect(theme).toBeVisible();
  await expect(endpoint).toBeVisible();
  await expect(page.locator(".return-live")).toBeVisible();
  await expect(page.getByRole("button", { name: "More controls" })).toHaveCount(
    0,
  );
  await expect(page.locator(".history-title svg")).toHaveCount(0);
  await expectNoHorizontalOverflow(page.locator(".topbar"));

  for (const expectedTheme of ["Light", "Dark"] as const) {
    while (
      !(
        await page
          .getByRole("button", { name: new RegExp(`^Theme: ${expectedTheme}`) })
          .state()
      ).some((state) => state.visible)
    )
      await theme.click();
    const styles = [];
    for (const control of [history, endpoint])
      styles.push(
        await control.evaluate((node) => {
          const style = getComputedStyle(node);
          return {
            background: style.backgroundColor,
            border: style.borderColor,
            color: style.color,
            shadow: style.boxShadow,
          };
        }),
      );
    expect(styles[0]).toEqual(styles[1]);
  }

  const management = page.getByRole("button", { name: "Archive management" });
  await management.click();
  const managementMenu = page.getByRole("menu", { name: "Archive management" });
  const managementBox = await managementMenu.boundingBox();
  expect(managementBox!.x).toBeGreaterThanOrEqual(0);
  expect(managementBox!.x + managementBox!.width).toBeLessThanOrEqual(390);
  await page.keyboard.press("Escape");
  await expect(management).toBeFocused();

  await page.setViewportSize({ width: 340, height: 640 });
  await expect(theme).toBeVisible();
  await expect(
    page.getByRole("button", { name: "More controls" }),
  ).toBeVisible();
  await expectNoHorizontalOverflow(page.locator(".topbar"));

  await page.setViewportSize({ width: 319, height: 640 });
  await expect(
    page.getByRole("button", { name: "More controls" }),
  ).toBeVisible();
  await expectNoHorizontalOverflow(page.locator(".topbar"));
});

test("authenticated phone chrome compacts account identity before core controls", async ({
  page,
}) => {
  await page.setViewportSize({ width: 390, height: 844 });
  const index = await Bun.file(
    new URL("../dist/index.html", import.meta.url),
  ).text();
  await page.route("**/index.html*", (route) =>
    route.fulfill({
      body: index.replace(
        "<head>",
        '<head><meta name="graphite-meter-auth" content="enabled">',
      ),
      headers: { "content-type": "text/html; charset=utf-8" },
    }),
  );
  await page.route("**/auth/session", (route) =>
    route.fulfill({
      json: {
        name: "A deliberately long authenticated account name",
        provider: "oidc",
        expires: "2026-08-31T12:00:00Z",
        csrf: "browser-test",
        remainingMs: 3_600_000,
        maximumLifetimeMs: 3_600_000,
      },
    }),
  );
  await page.goto("/index.html?engine=dummy");
  await setHistoryPreference(page, "enabled");

  await expect(
    page.locator('form[aria-label*="deliberately long authenticated"]'),
  ).toBeVisible();
  await expect(
    page.getByRole("button", { name: /Sign out .* everywhere/ }),
  ).toBeHidden();
  await expect(
    page.getByRole("button", {
      name: "Sign out A deliberately long authenticated account name",
      exact: true,
    }),
  ).toBeVisible();
  await expect(
    page.getByRole("button", { name: "Open History" }),
  ).toBeVisible();
  await expect(page.getByRole("button", { name: /^Theme:/ })).toBeVisible();
  await expect(
    page.getByRole("button", { name: "Toggle endpoint info" }),
  ).toBeVisible();
  await expect(page.getByRole("button", { name: "More controls" })).toHaveCount(
    0,
  );
  await expectNoHorizontalOverflow(page.locator(".topbar"));
});

test("sortable headers expose natural reversible order with missing values last", async ({
  page,
}) => {
  await openApp(page, "dummy", { width: 1024, height: 768 });
  const missing = record(IDS.middle, Date.UTC(2026, 7, 27, 12), 1);
  missing.stages.download = { status: "not-run", result: null };
  missing.totalBytes -= 9_999_999_999;
  const partialRecord = record(
    IDS.oldest,
    Date.UTC(2026, 7, 26, 12),
    10_000_000,
  );
  partialRecord.stages.upload = { status: "failed", result: null };
  partialRecord.failures = [
    { stage: "upload", direction: null, reason: "timeout" },
  ];
  partialRecord.totalBytes -= 9_999_999_999;
  await seedHistory(page, [
    partialRecord,
    missing,
    record(IDS.newest, Date.UTC(2026, 7, 28, 12), 50_000_000),
  ]);
  await openHistory(page);

  const partialRow = page.locator(`[data-history-id="${IDS.oldest}"]`);
  await expect(partialRow.getByText("Partial", { exact: true })).toHaveCount(1);
  await expect(
    partialRow.locator(".date-cell").getByText("Partial", { exact: true }),
  ).toHaveCount(1);

  const downloadHeader = page
    .getByRole("columnheader", { name: /Down/ })
    .getByRole("button");
  await downloadHeader.click();
  const ids = () =>
    page
      .locator(".result-row")
      .evaluateAll((rows) =>
        rows.map((row) => row.getAttribute("data-history-id")),
      );
  expect(await ids()).toEqual([IDS.newest, IDS.oldest, IDS.middle]);
  await downloadHeader.click();
  expect(await ids()).toEqual([IDS.oldest, IDS.newest, IDS.middle]);

  await partialRow.click();
  await expect(
    page.getByRole("heading", { name: "Stage issues" }),
  ).toBeVisible();
  await expect(page.locator(".issue-list li")).toHaveCount(1);
  await expect(page.locator(".result-detail details")).toHaveCount(0);

  await page.keyboard.press("Escape");
  await page.setViewportSize({ width: 390, height: 844 });
  await expect(partialRow.getByText("Partial", { exact: true })).toHaveCount(1);
  await expect(
    page
      .locator(`[data-history-id="${IDS.middle}"]`)
      .locator('.metric-cell[data-tone="download"]'),
  ).toContainText("Not run");
  await expect(partialRow.locator(".metric-cell small")).toHaveCount(4);
  expect(
    await partialRow.evaluate((node) => node.getBoundingClientRect().height),
  ).toBeLessThanOrEqual(80);
  await page
    .getByRole("button", { name: "Choose history view and sort" })
    .click();
  await page.getByRole("radio", { name: "Date" }).click();
  await page.getByRole("button", { name: "Date: Oldest first" }).click();
  await page.getByRole("checkbox", { name: "Upload" }).click();
  await page.keyboard.press("Escape");
  expect(await ids()).toEqual([IDS.oldest, IDS.middle, IDS.newest]);
  await expect(
    partialRow.locator('.metric-cell[data-tone="upload"]'),
  ).toHaveCount(0);
  await expectNoHorizontalOverflow(page.locator(".history-workspace"));
});

test("column visibility persists and narrow cards retain all six sort choices", async ({
  page,
}) => {
  await openApp(page, "dummy", { width: 1024, height: 768 });
  await seedHistory(page, [record(IDS.newest, Date.UTC(2026, 7, 28, 12))]);
  await openHistory(page);
  await page.getByRole("button", { name: "Choose visible columns" }).click();
  await expect(
    page.locator(".view-popover").getByRole("checkbox").first(),
  ).toBeFocused();
  await expect(page.getByRole("checkbox", { name: "Status" })).toHaveCount(0);
  await expect(page.locator(".view-popover").getByRole("checkbox")).toHaveCount(
    5,
  );
  const bidi = page.getByRole("checkbox", { name: "Bidirectional" });
  await expect(bidi).toHaveAttribute("aria-checked", "false");
  await bidi.click();
  await expect(
    page.getByRole("columnheader", { name: /Bi-dir/ }),
  ).toBeVisible();
  await page.waitForTimeout(350);
  await page.reload();
  await expect(
    page.getByRole("columnheader", { name: /Bi-dir/ }),
  ).toBeVisible();

  await page.setViewportSize({ width: 390, height: 844 });
  await page
    .getByRole("button", { name: "Choose history view and sort" })
    .click();
  await expect(page.locator(".view-popover").getByRole("radio")).toHaveCount(6);
  await expectNoHorizontalOverflow(page.locator(".view-popover"));
  const trigger = page.getByRole("button", {
    name: "Choose history view and sort",
  });
  const triggerBox = await trigger.boundingBox();
  const iconBox = await trigger.locator(".layout-icon svg").boundingBox();
  expect(
    Math.abs(
      triggerBox!.y +
        triggerBox!.height / 2 -
        (iconBox!.y + iconBox!.height / 2),
    ),
  ).toBeLessThanOrEqual(1);

  const popoverBox = await page.locator(".view-popover").boundingBox();
  expect(popoverBox!.x).toBeGreaterThanOrEqual(0);
  expect(popoverBox!.x + popoverBox!.width).toBeLessThanOrEqual(390);
  const loadedSort = page.getByRole("radio", { name: "Loaded" });
  await loadedSort.scrollIntoViewIfNeeded();
  const loadedBox = await loadedSort.boundingBox();
  expect(loadedBox!.x).toBeGreaterThanOrEqual(popoverBox!.x);
  expect(loadedBox!.x + loadedBox!.width).toBeLessThanOrEqual(
    popoverBox!.x + popoverBox!.width,
  );
  const directionCases = [
    ["Date", "Newest first", "Oldest first"],
    ["Download", "Fastest first", "Slowest first"],
    ["Upload", "Fastest first", "Slowest first"],
    ["Bidirectional", "Fastest first", "Slowest first"],
    ["Idle", "Lowest first", "Highest first"],
    ["Loaded", "Lowest first", "Highest first"],
  ] as const;
  for (const [field, natural, reverse] of directionCases) {
    const fieldOption = page.getByRole("radio", { name: field });
    await fieldOption.scrollIntoViewIfNeeded();
    await fieldOption.click();
    await expect(fieldOption).toHaveAttribute("aria-checked", "true");
    const naturalDirection = page.getByRole("button", {
      name: `${field}: ${natural}`,
    });
    const reverseDirection = page.getByRole("button", {
      name: `${field}: ${reverse}`,
    });
    await expect(naturalDirection).toHaveAttribute("aria-pressed", "true");
    await reverseDirection.click();
    await expect(reverseDirection).toHaveAttribute("aria-pressed", "true");
    await expect(naturalDirection).toHaveAttribute("aria-pressed", "false");
  }

  await page.keyboard.press("Escape");
  await page.setViewportSize({ width: 390, height: 640 });
  await trigger.click();
  const shortPopover = page.locator(".view-popover");
  await expect(shortPopover).toBeVisible();
  const lastColumn = shortPopover
    .getByRole("checkbox", { name: "Loaded latency" })
    .first();
  await lastColumn.scrollIntoViewIfNeeded();
  const shortBox = await shortPopover.boundingBox();
  const lastColumnBox = await lastColumn.boundingBox();
  expect(shortBox!.x).toBeGreaterThanOrEqual(0);
  expect(shortBox!.x + shortBox!.width).toBeLessThanOrEqual(390);
  expect(shortBox!.y + shortBox!.height).toBeLessThanOrEqual(640);
  expect(lastColumnBox!.x).toBeGreaterThanOrEqual(shortBox!.x);
  expect(lastColumnBox!.x + lastColumnBox!.width).toBeLessThanOrEqual(
    shortBox!.x + shortBox!.width,
  );
});

test("one inline scroll owner accepts wheel gestures over rows and mobile detail", async ({
  page,
}) => {
  await openApp(page, "dummy", { width: 844, height: 390 });
  const base = Date.UTC(2026, 7, 28, 12);
  await seedHistory(
    page,
    Array.from({ length: 60 }, (_, index) =>
      record(
        `00000000-0000-4000-8000-${index.toString(16).padStart(12, "0")}`,
        base - index * 60_000,
      ),
    ),
  );
  await openHistory(page);
  const scrollOwner = page.locator(".workspace-body");
  const row = page.locator(".result-row").first();
  await row.hover();
  const box = await row.boundingBox();
  const cdp = await page.context.newCDPSession(page);
  await cdp.send("Input.dispatchMouseEvent", {
    type: "mouseWheel",
    x: (box?.x ?? 0) + (box?.width ?? 0) / 2,
    y: (box?.y ?? 0) + (box?.height ?? 0) / 2,
    deltaX: 0,
    deltaY: 500,
  });
  await expect
    .poll(() => scrollOwner.evaluate((node) => node.scrollTop))
    .toBeGreaterThan(0);
  expect(
    await page
      .locator(".archive-list")
      .evaluate((node) =>
        ["auto", "scroll"].includes(getComputedStyle(node).overflowY),
      ),
  ).toBe(false);

  await row.click();
  const detailSurface = page.locator(".result-detail .throughput-card").first();
  await detailSurface.scrollIntoViewIfNeeded();
  await scrollOwner.evaluate((node) => {
    node.scrollTop = Math.max(0, node.scrollTop - 64);
  });
  const beforeDetailWheel = await scrollOwner.evaluate(
    (node) => node.scrollTop,
  );
  const detailBox = await scrollOwner.boundingBox();
  await page.mouse.move(
    (detailBox?.x ?? 0) + (detailBox?.width ?? 0) / 2,
    (detailBox?.y ?? 0) + (detailBox?.height ?? 0) / 2,
  );
  await cdp.send("Input.dispatchMouseEvent", {
    type: "mouseWheel",
    x: (detailBox?.x ?? 0) + (detailBox?.width ?? 0) / 2,
    y: (detailBox?.y ?? 0) + (detailBox?.height ?? 0) / 2,
    deltaX: 0,
    deltaY: 500,
  });
  await expect
    .poll(() => scrollOwner.evaluate((node) => node.scrollTop))
    .toBeGreaterThan(beforeDetailWheel);
  expect(
    await page
      .locator(".detail-inspector")
      .evaluateAll((nodes) =>
        nodes.some((node) =>
          ["auto", "scroll"].includes(getComputedStyle(node).overflowY),
        ),
      ),
  ).toBe(false);
});

test("wide list and detail scroll independently under an opaque sticky header", async ({
  page,
}) => {
  await openApp(page, "dummy", { width: 1366, height: 640 });
  const base = Date.now();
  const values = Array.from({ length: 80 }, (_, index) =>
    record(
      `00000000-0000-4000-8000-${index.toString(16).padStart(12, "0")}`,
      base - index * 60_000,
    ),
  );
  await seedHistory(page, values);
  await openHistory(page);

  const list = page.locator(".archive-list");
  const detail = page.locator(".detail-inspector");
  await expect(list).toBeVisible();
  await expect(detail).toHaveCount(0);
  await list.evaluate((node) => (node.scrollTop = 300));
  await page.locator(".result-row").nth(8).click();
  await expect(detail).toBeVisible();
  expect(await list.evaluate((node) => node.scrollTop)).toBeGreaterThan(0);
  const body = page.locator(".workspace-body.with-side");
  expect(await list.evaluate((node) => getComputedStyle(node).overflowY)).toBe(
    "auto",
  );
  expect(
    await detail.evaluate((node) => getComputedStyle(node).overflowY),
  ).toBe("auto");
  expect(await body.evaluate((node) => getComputedStyle(node).overflowY)).toBe(
    "hidden",
  );

  await list.evaluate((node) => (node.scrollTop = 0));
  await detail.evaluate((node) => (node.scrollTop = 0));
  const cdp = await page.context.newCDPSession(page);
  const row = page.locator(".result-row").first();
  await row.hover();
  const rowBox = await row.boundingBox();
  await cdp.send("Input.dispatchMouseEvent", {
    type: "mouseWheel",
    x: (rowBox?.x ?? 0) + (rowBox?.width ?? 0) / 2,
    y: (rowBox?.y ?? 0) + (rowBox?.height ?? 0) / 2,
    deltaX: 0,
    deltaY: 500,
  });
  await expect
    .poll(() => list.evaluate((node) => node.scrollTop))
    .toBeGreaterThan(0);
  expect(await detail.evaluate((node) => node.scrollTop)).toBe(0);
  const listAfterWheel = await list.evaluate((node) => node.scrollTop);

  const phase = detail.locator(".throughput-card").first();
  await phase.hover();
  const phaseBox = await phase.boundingBox();
  await cdp.send("Input.dispatchMouseEvent", {
    type: "mouseWheel",
    x: (phaseBox?.x ?? 0) + (phaseBox?.width ?? 0) / 2,
    y: (phaseBox?.y ?? 0) + (phaseBox?.height ?? 0) / 2,
    deltaX: 0,
    deltaY: 500,
  });
  await expect
    .poll(() => detail.evaluate((node) => node.scrollTop))
    .toBeGreaterThan(0);
  expect(await list.evaluate((node) => node.scrollTop)).toBe(listAfterWheel);

  const header = page.locator(".column-head");
  const headerState = await header.evaluate((node) => {
    const box = node.getBoundingClientRect();
    const hit = document.elementFromPoint(
      box.left + box.width / 2,
      box.top + box.height / 2,
    );
    return {
      background: getComputedStyle(node).backgroundColor,
      ownsHit: hit === node || node.contains(hit),
    };
  });
  expect(headerState.background).not.toBe("rgba(0, 0, 0, 0)");
  expect(headerState.ownsHit).toBe(true);
});

test("archive stays chunked and overflow-free across required shell geometries", async ({
  page,
}) => {
  await openApp(page, "dummy", { width: 1366, height: 768 });
  const base = Date.UTC(2026, 7, 28, 12);
  const values = Array.from({ length: 120 }, (_, index) =>
    record(
      `00000000-0000-4000-8000-${index.toString(16).padStart(12, "0")}`,
      base - index * 60_000,
      125_000_000 - index,
    ),
  );
  await seedHistory(page, values);
  await openHistory(page, values[0].id);
  await expect(page.locator(".result-row")).toHaveCount(50);
  await expect(
    page.getByRole("button", { name: "Archive management" }),
  ).toBeVisible();

  await page.emulateMedia({ reducedMotion: "reduce" });
  expect(
    await page
      .locator(".history-workspace")
      .evaluate((node) => getComputedStyle(node).animationName),
  ).toBe("none");
  expect(
    Number.parseFloat(
      await page
        .locator(".result-row")
        .first()
        .evaluate((node) => getComputedStyle(node).transitionDuration),
    ),
  ).toBeLessThan(0.001);
  const viewControl = page.getByRole("button", {
    name: /Choose (visible columns|history view and sort)/,
  });
  await viewControl.click();
  expect(
    await page
      .locator(".view-popover")
      .evaluate((node) => getComputedStyle(node).animationName),
  ).toBe("none");
  await page.keyboard.press("Escape");

  const viewports = [
    { width: 390, height: 640 },
    { width: 390, height: 844 },
    { width: 844, height: 390 },
    { width: 768, height: 1024 },
    { width: 1024, height: 768 },
    { width: 1366, height: 768 },
    { width: 1440, height: 900 },
  ];
  for (const viewport of viewports) {
    await page.setViewportSize(viewport);
    await page.waitForTimeout(80);
    await expectNoHorizontalOverflow(page.locator("body"));
    await expectNoHorizontalOverflow(page.locator(".history-workspace"));
  }

  await page.locator(".load-more button").evaluate((button) => button.click());
  await expect(page.locator(".result-row")).toHaveCount(100);
  await expect(page.locator(".archive-management")).toHaveCount(0);
  await page.locator(".load-more button").evaluate((button) => button.click());
  await expect(page.locator(".result-row")).toHaveCount(120);
  await expect(
    page.getByRole("button", { name: "Archive management" }),
  ).toBeVisible();
});

test("saved incomplete probe accounting remains visible with no known outcomes", async ({
  page,
}) => {
  await openApp(page, "dummy", { width: 1366, height: 768 });
  const partial = record(IDS.newest, Date.UTC(2026, 7, 28, 12));
  partial.schemaVersion = 2;
  for (const snapshot of [
    partial.stages.download.result,
    partial.stages.upload.result,
    partial.stages.bidirectional.down,
    partial.stages.bidirectional.up,
    partial.stages.latency.result,
  ]) {
    if (!snapshot) continue;
    snapshot.probeTimeoutPct = snapshot.packetLossPct ?? null;
    delete snapshot.packetLossPct;
  }
  partial.stages.latency.lanes = {
    latency: null,
    upload: null,
    bidirectional: null,
    download: {
      min: null,
      max: null,
      p10: null,
      p90: null,
      center: null,
      jitter: null,
      count: 0,
      timeoutRatio: null,
      timeoutCount: 0,
      unresolvedCount: 0,
      sendFailureCount: 0,
      accountingComplete: false,
    },
  };
  const earlierV2 = structuredClone(partial);
  earlierV2.id = IDS.middle;
  const earlierLane = earlierV2.stages.latency.lanes.download!;
  delete earlierLane.accountingComplete;
  delete earlierLane.timeoutCount;
  earlierLane.count = 10;
  earlierLane.timeoutRatio = 0.1;
  await seedHistory(page, [partial, earlierV2]);
  await openHistory(page, partial.id);
  const profile = page.locator(
    '[data-latency-profile][data-variant="compact"]',
  );
  await expect(profile.getByText("Partial accounting")).toBeVisible();
  await expect(profile).toContainText("Additional outcomes unknown.");
  await expect(profile).toContainText(
    "0 resolved · 0 timeouts · 0 unresolved · 0 send failures",
  );
  const note = profile.getByRole("note");
  await note.focus();
  await expect(page.getByRole("tooltip")).toContainText(
    "Worker shutdown could not account for all probes",
  );
  const timeouts = page.locator(".probe-timeouts-section");
  await expect(timeouts).toBeVisible();
  await expect(timeouts.locator("li em")).toHaveText("Partial");
  await page.setViewportSize({ width: 390, height: 844 });
  await expect(profile.getByText("Partial accounting")).toBeVisible();
  await expectNoHorizontalOverflow(profile);
  await page.evaluate((id) => {
    window.location.hash = `/history/${id}`;
  }, earlierV2.id);
  await expect(profile.getByText("Partial accounting")).toBeVisible();
  await expect(profile).toContainText("Known: 10 resolved");
  await expect(profile).not.toContainText("0 timeouts");
  await profile.getByRole("note").focus();
  await expect(page.getByRole("tooltip")).toContainText(
    "predates probe-accounting completeness metadata",
  );
});
