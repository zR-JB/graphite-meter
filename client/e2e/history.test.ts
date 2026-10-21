import { upgradeRecord } from "../src/app/history/records";
import {
  countSaves,
  home,
  latencyOnly,
  open,
  phase,
  ready,
  run,
  runButton,
  HISTORY_DB,
} from "./fleet";
import { expect, test, type Page } from "./webview";

const id = (index: number) => `00000000-0000-4000-8000-${index.toString(16).padStart(12, "0")}`;
const base = Date.UTC(2026, 7, 28, 12);

/** A row as schema 4 saved it, before database version 3. */
function legacy(index: number, completedAt = base - index * 60_000) {
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

const record = (index: number, completedAt?: number) => upgradeRecord(legacy(index, completedAt))!;

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
          database.createObjectStore(db.metadataStore, { keyPath: db.metadataKeyPath });
        };
        opening.onerror = () => reject(opening.error);
        opening.onsuccess = () => {
          const database = opening.result;
          const stores = [...database.objectStoreNames];
          const tx = database.transaction(stores, "readwrite");
          for (const value of records) tx.objectStore(db.resultsStore).put(value);
          if (clears !== undefined)
            tx.objectStore(db.metadataStore).put({
              key: db.clearsKey,
              value: clears,
            });
          tx.oncomplete = () => {
            database.close();
            resolve();
          };
          tx.onerror = tx.onabort = () => reject(tx.error ?? new Error("seed aborted"));
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
          tx.onerror = tx.onabort = () => reject(tx.error ?? new Error("read aborted"));
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

const fixturePage = (page: Page) => open(page, `${home.url}/version.json`, { config: latencyOnly });
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
        // Interrupt the actual scan, without slowing cursor deliveries or relying on the driver to catch a brief state.
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
  expect(await page.evaluate(() => (window as any).archiveScan.deliveries)).toBe(
    stopped.deliveries,
  );
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
      page.evaluate(() => document.querySelector(".result-row")?.getAttribute("data-history-id")),
    )
    .toBe(id(1_999));
  await expect(rows).toHaveCount(50);

  await history(page, id(2_000));
  await expect(page.locator(".result-detail")).toBeVisible({ timeout: 15_000 });
  expect(await page.locator(".result-detail").textContent()).not.toContain("Grade");
  await expect(heading).toHaveText("2000");
});

test("a closed Columns popover never takes a tap meant for a result", async (page) => {
  await fixturePage(page);
  await seed(page, { records: [record(1)] });
  await page.setViewportSize({ width: 390, height: 844 });
  await history(page);
  const options = page.getByRole("dialog", { name: "History view options" });
  await page.getByRole("button", { name: "Columns", exact: true }).click();
  await expect(options).toBeVisible();
  await page.press("Escape");
  await page.locator(".result-row").click();
  await expect(page.locator(".result-detail")).toBeVisible();
});

test("folded at phone width, another order names itself and Sort by date brings the days back", async (page) => {
  await fixturePage(page);
  await seed(page, { records: [record(1), record(2, Date.now())] });
  await page.setViewportSize({ width: 375, height: 812 });
  await history(page);
  const headings = () =>
    page.locator(".history-table h3").all((els: Element[]) => els.map((el) => el.textContent));
  await expect.poll(headings).toHaveLength(2);
  const days = await headings();
  await page.locator('.column-head button:has([data-tone="idle"])').click();
  await expect.poll(headings).toEqual(["Idle latency, lowest first"]);
  await page.getByRole("button", { name: "Sort by date", exact: true }).click();
  await expect.poll(headings).toEqual(days);
});

test("Sort by is one tab stop whose arrows, Home and End choose the order", async (page) => {
  await fixturePage(page);
  await seed(page, { records: [record(1)] });
  await page.setViewportSize({ width: 1280, height: 800 });
  await history(page);
  await page.getByRole("button", { name: "Columns", exact: true }).click();
  await expect(page.getByRole("dialog", { name: "History view options" })).toBeVisible();
  // Each order: whether it is chosen, a tab stop and focused.
  const orders = () =>
    page.evaluate(() =>
      Array.from(
        document.querySelectorAll<HTMLElement>(
          '[role="radiogroup"][aria-label="Sort by"] [role="radio"]',
        ),
        (radio) =>
          [
            radio.getAttribute("aria-checked") === "true",
            radio.tabIndex === 0,
            radio === document.activeElement,
          ]
            .map(Number)
            .join(""),
      ),
    );
  const count = (await orders()).length;
  expect(count).toBeGreaterThan(2);
  const only = (at: number, state: string) =>
    Array.from({ length: count }, (_, index) => (index === at ? state : "000"));
  expect(await orders()).toEqual(only(0, "110"));
  // The popover focuses its first column; Tab passes the columns and the heading's term to the chosen order alone, then leaves.
  const role = () => page.evaluate(() => document.activeElement?.getAttribute("role"));
  await expect.poll(role).toBe("checkbox");
  for (let stop = 0; stop < 10 && (await role()) !== "radio"; stop++) await page.press("Tab");
  expect(await orders()).toEqual(only(0, "111"));
  for (const [key, at] of [
    ["ArrowDown", 1],
    ["ArrowRight", 2],
    ["End", count - 1],
    ["ArrowDown", 0],
    ["ArrowUp", count - 1],
    ["Home", 0],
  ] as const) {
    await page.press(key);
    expect(await orders()).toEqual(only(at, "111"));
  }
  await page.press("Tab");
  expect(await role()).not.toBe("radio");
});

test("a page shown again after a night away dates its results at once", async (page) => {
  await fixturePage(page);
  const at = await page.evaluate(() => {
    const now = Date.now();
    return Math.max(new Date(now).setHours(0, 0, 0, 0), now - 60_000);
  });
  await seed(page, { records: [record(1, at)] });
  await history(page);
  const heading = page.locator(".history-list h3");
  await expect(heading).toHaveText("Today");
  // The night passes in the background; the minute clock has not ticked since.
  await page.evaluate(() => {
    const real = Date.now;
    Date.now = () => {
      const next = new Date(real());
      return next.setDate(next.getDate() + 1);
    };
    document.dispatchEvent(new Event("visibilitychange"));
  });
  await expect(heading).toHaveText("Yesterday");
});

test("the keyboard moves the list's split within both panes' limits, and it survives a reload", async (page) => {
  await fixturePage(page);
  await seed(page, { records: [record(1)] });
  await page.setViewportSize({ width: 1280, height: 800 });
  await history(page, id(1));
  const handle = page.getByRole("separator", { name: /^Resize results list/ });
  const list = () => page.locator(".history-list").evaluate((el: HTMLElement) => el.offsetWidth);
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
      page.evaluate(() => JSON.parse(localStorage.getItem("graphite-meter:v1")!).historySplit),
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

test("version 2's readable rows are upgraded; unsupported and malformed rows are skipped, kept and clearable", async (page) => {
  await fixturePage(page);
  const readable = legacy(1);
  const old = [
    { ...legacy(2), schemaVersion: 1 },
    { ...legacy(3), schemaVersion: 2 },
    legacy(4, 1e20),
  ];
  const broken = legacy(5);
  Object.assign(broken.stages.latency.result, { reportedMs: "fast" });
  await seed(page, { records: [readable, broken, ...old], version: 2 });
  const before = await stored(page);
  await history(page);
  await expect(page.locator(".result-row")).toHaveCount(1);
  await expect(page.locator(".history-workspace")).toContainText(
    "4 unreadable results were skipped.",
  );
  const upgraded = {
    ...before,
    version: HISTORY_DB.version,
    records: before.records.map((row: any) => (row.id === readable.id ? upgradeRecord(row) : row)),
  };
  expect(await stored(page)).toEqual(upgraded);
  const unreadable = page.getByRole("heading", {
    name: "Unreadable saved result",
  });
  for (const id of [broken.id, old[0]!.id]) {
    await history(page, id);
    await expect(unreadable).toBeVisible();
  }
  await page.reload();
  await expect(unreadable).toBeVisible();
  expect(await stored(page)).toEqual(upgraded);

  const management = page.getByRole("button", { name: "History actions" });
  await management.click();
  await page.getByRole("menuitem", { name: "Clear History" }).click();
  await page
    .getByRole("alertdialog", { name: "Clear History?" })
    .getByRole("button", { name: "Clear History" })
    .click();
  await expect(page.getByRole("heading", { name: "No saved results" })).toBeVisible();
  expect((await stored(page)).records).toEqual([]);
});

const facts = (page: Page) =>
  page
    .locator(".detail-pane .facts > .group")
    .all((groups: HTMLElement[]) =>
      Object.fromEntries(
        groups.map((group) => [
          group.querySelector("h3")!.textContent,
          Object.fromEntries(
            [...group.querySelectorAll("dl > div")].map((row) => [
              row.querySelector("dt")!.textContent,
              row.querySelector("select")?.value ?? row.querySelector("dd")!.textContent,
            ]),
          ),
        ]),
      ),
    );

test("version 3's run-wide engine version moves to its latency server, and a result shows each server's facts", async (page) => {
  await fixturePage(page);
  const current = record(1);
  const run = current.result.multiServer;
  const { engineVersion, ...self } = run.servers[0]!;
  expect(engineVersion).toBe("e2e");
  const oslo = {
    ...self,
    server: { id: "oslo", name: "Oslo", url: "https://oslo.example" },
    throughput: { ...self.throughput, clientIpVersion: 4 },
  };
  const servers = [self, oslo];
  const schema5 = {
    ...current,
    schemaVersion: 5,
    engine: "0.9.1",
    result: {
      ...current.result,
      multiServer: {
        ...run,
        selection: servers.map(({ server }) => server),
        participants: servers.map(({ server }) => server.id),
        latencyFocus: "oslo",
        servers,
      },
    },
  };
  await seed(page, { records: [schema5], version: 3 });
  await history(page, schema5.id);
  const upgraded = upgradeRecord(schema5)!;
  expect(upgraded.result.multiServer.servers.map((each) => each.engineVersion)).toEqual([
    undefined,
    "0.9.1",
  ]);
  await expect
    .poll(() => stored(page))
    .toEqual({ records: [upgraded], clears: undefined, version: HISTORY_DB.version });

  const shows = (server: string, ip: string, build: object) =>
    expect
      .poll(() => facts(page))
      .toMatchObject({
        "Test server": { Server: server, "IP family": ip },
        Build: { Client: "e2e", ...build },
      });
  await shows("oslo", "IPv4", { Server: "0.9.1" });
  await page
    .locator(".detail-pane .facts")
    .getByRole("combobox", { name: "Server", exact: true })
    .fill("self");
  await shows("self", "IPv6", {});
  expect((await facts(page)).Build).toEqual({ Client: "e2e" });
  const lens = page.getByRole("combobox", { name: "Servers shown in this result" });
  await lens.fill("self");
  await lens.fill("");
  await shows("oslo", "IPv4", { Server: "0.9.1" });
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
  await expect.poll(async () => (await stored(page)).records.length, { timeout: 20_000 }).toBe(4);
  await history(page);
  await expect(page.locator(".result-row")).toHaveCount(3);
  await expect(page.locator(".history-workspace")).toContainText(
    "1 unreadable result was skipped.",
  );
});

test("History refuses other database versions at once and on Retry, and never changes them", async (page) => {
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
  await page.evaluate(() => new BroadcastChannel("graphite-meter-history").postMessage(""));
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
  // An open cannot be canceled: read once History's queued upgrade has been refused and left version 1.
  await expect
    .poll(
      async () => {
        const { databases, opens } = await page.storage();
        const settled = /^(success|error)/;
        const pending = opens.filter(({ events }) => !events.some((event) => settled.test(event)));
        return { databases, pending };
      },
      { timeout: 10_000 },
    )
    .toEqual({
      databases: [{ name: HISTORY_DB.name, version: 1 }],
      pending: [],
    });
  expect(await stored(page)).toEqual(before);
  // Chrome 153's renderer crashes when an open races an aborting upgrade, so read once Retry's open has settled.
  const tried = (await page.storage()).opens.length;
  await page.getByRole("button", { name: "Retry", exact: true }).click();
  await expect
    .poll(async () => {
      const { opens } = await page.storage();
      return (
        opens.length > tried &&
        opens.every(({ events }) => events.some((event) => /^(success|error)/.test(event)))
      );
    }, OPEN_BOUND)
    .toBe(true);
  await expect(refusal).toBeVisible(OPEN_BOUND);
  expect(await stored(page)).toEqual(before);
});

test("History never waits on a version 2 connection, and upgrades once that connection closes", async (page) => {
  const refusal = page.getByRole("heading", { name: "History is unavailable" });
  await fixturePage(page);
  const row = legacy(1);
  await seed(page, { records: [row], version: 2 });
  await page.goto(home.url);
  // An older build's tab keeps its connection open: the upgrade is blocked, and History says so within a moment.
  const older = await page.evaluate(
    (db) =>
      new Promise<string>((resolve) => {
        const opening = indexedDB.open(db.name);
        opening.onerror = () => resolve(`failed: ${opening.error?.name}`);
        opening.onsuccess = () => {
          (window as any).older = opening.result;
          resolve(`v${opening.result.version}`);
        };
      }),
    HISTORY_DB,
  );
  expect(older).toBe("v2");
  await page.evaluate(() => (location.hash = "#/history"));
  await expect(refusal).toBeVisible(OPEN_BOUND);
  // The blocked open stays queued: once the older connection closes it upgrades the row in place.
  await page.evaluate(() => (window as any).older.close());
  await expect
    .poll(async () => {
      const { databases, opens } = await page.storage();
      const pending = opens.filter(
        ({ events }) => !events.some((event) => /^(success|error)/.test(event)),
      );
      return { databases, pending };
    }, OPEN_BOUND)
    .toEqual({ databases: [{ name: HISTORY_DB.name, version: HISTORY_DB.version }], pending: [] });
  expect((await stored(page)).records).toEqual([upgradeRecord(row)]);
  await page.getByRole("button", { name: "Retry", exact: true }).click();
  await expect(page.locator(".result-row")).toHaveCount(1);
});

test("a result History refused to save says so, and starting over, a new run or leaving asks first", async (page) => {
  await fixturePage(page);
  await seed(page, { records: [], version: HISTORY_DB.version + 1 });
  await page.goto(home.url);
  await ready(page);
  await runButton(page, "Start test").click();
  await expect(phase(page, "complete")).toHaveCount(1, { timeout: 20_000 });
  // The warning stays beside the result, past any toast's reading time.
  const notice = page.locator(".phase-toast.visible");
  await expect(notice).toContainText("Not saved", { timeout: 8_000 });
  const ask = page.getByRole("alertdialog", { name: "Clear this result?" });
  await page.getByRole("button", { name: /start over$/ }).click();
  await expect(ask).toBeVisible();
  await ask.getByRole("button", { name: "Keep result" }).click();
  const replace = page.getByRole("alertdialog", { name: "Replace this result?" });
  await runButton(page, "Run again").click();
  await expect(replace).toBeVisible();
  await replace.getByRole("button", { name: "Keep result" }).click();
  await expect(runButton(page, "Run again")).toBeVisible();
  const leaving = () =>
    page.evaluate(() => {
      const event = new Event("beforeunload", { cancelable: true });
      window.dispatchEvent(event);
      return event.defaultPrevented;
    });
  expect(await leaving()).toBe(true);
  await runButton(page, "Run again").click();
  await replace.getByRole("button", { name: "Run again" }).click();
  await expect(phase(page, "complete")).toHaveCount(0);
});

/** Holds History's read of each result listed in `window.hold` until `release`, once the read is under way. */
const holdReads = (page: Page) =>
  page.addInitScript(() => {
    const w = window as any;
    w.hold = new Set<string>();
    const held: (() => void)[] = [];
    w.waiting = () => held.length;
    w.release = () => {
      w.hold.clear();
      for (const deliver of held.splice(0)) deliver();
    };
    const get = IDBObjectStore.prototype.get;
    IDBObjectStore.prototype.get = function (this: IDBObjectStore, key: IDBValidKey | IDBKeyRange) {
      const real = get.call(this, key);
      if (!w.hold.has(key)) return real;
      const late: any = { result: undefined, error: null, onsuccess: null, onerror: null };
      real.onsuccess = () => {
        late.result = real.result;
        held.push(() => late.onsuccess?.());
      };
      real.onerror = () => {
        late.error = real.error;
        held.push(() => late.onerror?.());
      };
      return late;
    };
  });

async function release(page: Page) {
  await expect.poll(() => page.evaluate(() => (window as any).waiting())).toBe(1);
  await page.evaluate(() => (window as any).release());
}

test("a result kept on show while another is read takes no action, and Delete names its result", async (page) => {
  await holdReads(page);
  await fixturePage(page);
  await seed(page, { records: [record(1), record(2)] });
  await history(page, id(1));
  await expect(page.locator(".result-detail")).toHaveCount(1, OPEN_BOUND);
  await page.evaluate((held) => (window as any).hold.add(held), id(2));
  await page.locator(`a[data-history-id="${id(2)}"]`).click();
  // The first result stays on show while the second is read, and its Delete belongs to it alone.
  await expect(page.locator(".detail-pane[inert] .result-detail")).toHaveCount(1);
  await page
    .locator('.detail-pane button[aria-label="Delete this result"]')
    .evaluate((button: HTMLElement) => button.click());
  await expect(page.getByRole("alertdialog")).toHaveCount(0);
  await release(page);
  await expect(page.locator(".detail-pane:not([inert]) .result-detail")).toHaveCount(1);
  await page.getByRole("button", { name: "Delete this result" }).click();
  const prompt = page.getByRole("alertdialog", { name: "Delete this result?" });
  const when = await page.evaluate(
    (at) =>
      new Intl.DateTimeFormat(undefined, {
        weekday: "short",
        month: "short",
        day: "numeric",
        year: "numeric",
        hour: "2-digit",
        minute: "2-digit",
      }).format(at),
    base - 2 * 60_000,
  );
  await expect(prompt).toContainText(when);
  await prompt.getByRole("button", { name: "Delete result" }).click();
  await expect
    .poll(async () => (await stored(page)).records.map((row: any) => row.id))
    .toEqual([id(1)]);
});

test("a result read late takes focus only while focus stays where it was chosen", async (page) => {
  await holdReads(page);
  await fixturePage(page);
  await seed(page, { records: [record(1), record(2), record(3)] });
  await history(page, id(1));
  await expect(page.locator(".result-detail")).toHaveCount(1, OPEN_BOUND);
  for (const [result, away] of [
    [id(2), false],
    [id(3), true],
  ] as const) {
    await page.evaluate((held) => (window as any).hold.add(held), result);
    await page.locator(`a[data-history-id="${result}"]`).click();
    // Focus moves to a control outside History before the read lands.
    if (away)
      await page
        .getByRole("button", { name: "Settings", exact: true })
        .evaluate((key: HTMLElement) => key.focus());
    await release(page);
    await expect(page.locator(".detail-pane:not([inert]) .result-detail")).toHaveCount(1);
    await expect
      .poll(() =>
        page.evaluate(
          () =>
            document.activeElement?.getAttribute("aria-label") ?? document.activeElement?.className,
        ),
      )
      .toBe(away ? "Settings" : "result-detail");
  }
});

test("Up and Down open the results beside the open one on its history entry, past the first page", async (page) => {
  await fixturePage(page);
  await seed(page, { records: Array.from({ length: 52 }, (_, index) => record(index + 1)) });
  await page.setViewportSize({ width: 1280, height: 800 });
  await history(page, id(50));
  await expect(page.locator(".result-detail")).toHaveCount(1, OPEN_BOUND);
  const entries = await page.evaluate(() => history.length);
  const open = () =>
    page.evaluate(() => decodeURIComponent(location.hash.replace("#/history/", "")));
  for (const [key, expected] of [
    ["ArrowDown", id(51)],
    ["ArrowUp", id(50)],
    ["ArrowUp", id(49)],
  ] as const) {
    await page.press(key);
    await expect.poll(open).toBe(expected);
    await expect(page.locator(`.detail-pane:not([inert]) .result-detail`)).toHaveCount(1);
  }
  // The row past the first page was built, and the open row stays in the list's view.
  const row = page.locator(`a[data-history-id="${id(49)}"]`);
  await expect(row).toHaveAttribute("aria-current", "true");
  await expect
    .poll(() =>
      row.evaluate((link: HTMLElement) => {
        const box = link.getBoundingClientRect();
        const list = link.closest(".history-list")!.getBoundingClientRect();
        return box.top >= list.top && box.bottom <= list.bottom;
      }),
    )
    .toBe(true);
  await expect(page.locator(`a[data-history-id="${id(51)}"]`)).toHaveCount(1);
  expect(await page.evaluate(() => history.length)).toBe(entries);
  // Closing steps back to the list, as from the result first opened.
  await page.press("Escape");
  await expect.poll(() => page.evaluate(() => location.hash)).toBe("#/history");
  await expect(row).toBeFocused();
  await history(page, id(1));
  await expect(page.locator(".result-detail")).toHaveCount(1, OPEN_BOUND);
  // Past the newest result Up does nothing, so Down then opens the second.
  await page.press("ArrowUp");
  await page.press("ArrowDown");
  await expect.poll(open).toBe(id(2));
});

test("closing a linked result far down the list returns to its row", async (page) => {
  await fixturePage(page);
  await seed(page, { records: Array.from({ length: 60 }, (_, index) => record(index + 1)) });
  for (const [width, close] of [
    [1280, "Close result"],
    [700, "Results"],
  ] as const) {
    await page.setViewportSize({ width, height: 800 });
    await history(page, id(60));
    await expect(page.locator(".result-detail")).toHaveCount(1, OPEN_BOUND);
    await page.getByRole("button", { name: close, exact: true }).click();
    const row = page.locator(`a[data-history-id="${id(60)}"]`);
    await expect(row).toBeFocused();
    expect(
      await row.evaluate((link: HTMLElement) => {
        const box = link.getBoundingClientRect();
        const list = link.closest(".history-list")!.getBoundingClientRect();
        return box.top >= list.top && box.bottom <= list.bottom;
      }),
    ).toBe(true);
  }
});
