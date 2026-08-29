import type { HistoryRecordV1 } from "../src/lib/history/types";
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
): HistoryRecordV1 {
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
      latency: { protocol: "h3", kind: "webtransport-datagram" },
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

async function seedHistory(page: TestPage, values: unknown[], notify = true) {
  await page.evaluate(
    (input) =>
      new Promise<void>((resolve, reject) => {
        const opening = indexedDB.open("graphite-meter", 1);
        opening.onupgradeneeded = () => {
          const store = opening.result.createObjectStore("results", {
            keyPath: "id",
          });
          store.createIndex("completedAt", "completedAt");
        };
        opening.onerror = () => reject(opening.error);
        opening.onsuccess = () => {
          const db = opening.result;
          const transaction = db.transaction("results", "readwrite");
          const store = transaction.objectStore("results");
          store.clear();
          for (const item of input.items) store.put(item);
          transaction.oncomplete = () => {
            db.close();
            if (input.notify)
              window.dispatchEvent(new Event("graphite-meter-history-changed"));
            resolve();
          };
          transaction.onerror = () => reject(transaction.error);
        };
      }),
    { items: values, notify },
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

test("explicitly enabled completion is reachable in History and stays responsive", async ({
  page,
}) => {
  await openApp(page, "dummy", { width: 390, height: 844 });
  const settings = await openSettings(page);
  await settings
    .getByText("Save completed results on this device", { exact: true })
    .click();
  await settings.getByRole("button", { name: "short", exact: true }).click();
  await settings.getByRole("button", { name: "Close Settings" }).click();
  await startTest(page);
  await waitForCompletion(page, 20_000);
  await toggleHistoryFromTopbar(page);
  await expect(page.getByRole("button", { name: "More controls" })).toHaveClass(
    /active/,
  );
  await expect(page.getByRole("button", { name: /^Theme:/ })).toBeVisible();
  await expectNoHorizontalOverflow(page.locator(".topbar"));
  await expect(page.getByRole("heading", { name: "History" })).toBeVisible();
  await expect(page.locator(".overview-primary")).toContainText("1");
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
    page.getByRole("button", { name: "More controls" }),
  ).toBeVisible();
  await expect(page.getByRole("button", { name: "Open History" })).toHaveCount(
    0,
  );
  await page.getByRole("button", { name: "More controls" }).click();
  await expect(
    page.getByRole("menuitem", { name: /Open History/ }),
  ).toBeVisible();
  await expect(
    page.getByRole("menuitem", { name: /Endpoint info/ }),
  ).toBeVisible();
  await expect(page.getByRole("menuitem", { name: /Theme:/ })).toHaveCount(0);
  await page.keyboard.press("Escape");
  await expectNoHorizontalOverflow(page.locator(".topbar"));

  await page.setViewportSize({ width: 700, height: 844 });
  await expect(
    page.getByRole("button", { name: "Open History" }),
  ).toBeVisible();
  await expect(page.getByRole("button", { name: /^Theme:/ })).toBeVisible();
  await page.getByRole("button", { name: "More controls" }).click();
  await expect(
    page.getByRole("menuitem", { name: /Endpoint info/ }),
  ).toBeVisible();
  await expect(
    page.getByRole("menuitem", { name: /Open History/ }),
  ).toHaveCount(0);
  await page.keyboard.press("Escape");

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

test("malformed-only archives keep a raw clear path", async ({ page }) => {
  await openApp(page, "dummy", { width: 900, height: 700 });
  await seedHistory(page, [{ id: "malformed", unexpected: true }]);
  await openHistory(page);
  await expect(page.getByText(/1 malformed record was ignored/)).toBeVisible();
  const clear = page.getByRole("button", { name: "Clear all saved results" });
  await expect(clear).toBeVisible();
  await clear.click();
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
    () =>
      new Promise<number>((resolve, reject) => {
        const request = indexedDB.open("graphite-meter", 1);
        request.onerror = () => reject(request.error);
        request.onsuccess = () => {
          const tx = request.result.transaction("results", "readonly");
          const get = tx.objectStore("results").count();
          get.onsuccess = () => resolve(get.result);
        };
      }),
  );
  expect(count).toBe(0);
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
  await settings.getByRole("link", { name: "Manage History" }).click();
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
  await page.keyboard.press("Escape");

  await page.setViewportSize({ width: 390, height: 844 });
  await expect(settingsPanel(page)).toHaveAttribute("inert", "");
  await expect(endpointPanel(page)).toBeVisible();
  expect(await page.evaluate(() => window.location.hash)).toBe(
    "#/history?panels=endpoint",
  );

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

test("deep-linked detail is a focused side inspector and a focused inline expansion", async ({
  page,
}) => {
  await openApp(page, "dummy", { width: 1440, height: 900 });
  await seedHistory(page, [record(IDS.newest, Date.UTC(2026, 7, 28, 12))]);
  await openHistory(page, IDS.newest);
  const heading = page.locator(".result-detail h2");
  await expect(page.locator(".detail-inspector")).toBeVisible();
  await expect(heading).toBeFocused();
  const detail = page.locator(".detail-inspector");
  await expect(detail.locator(".profile-lane")).toHaveCount(4);
  await expect(detail.getByText("P50", { exact: true })).toBeVisible();
  await expect(detail.getByText("P95", { exact: true })).toBeVisible();
  await expect(detail.getByText("Stability", { exact: true })).toBeVisible();
  await expect(detail.locator(".bufferbloat-band")).toHaveCount(0);
  await expect(detail.locator("summary.section-head")).toHaveCount(4);
  await expect(detail.locator(".disclosure")).toHaveCount(0);
  await expect(detail.locator(".phase-row")).toHaveCount(3);
  await expect(detail.locator(".phase-row")).not.toContainText("loss");
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
  await expect(heading).toBeFocused();
  await expect(row.getByText("Selected", { exact: true })).toBeVisible();
  await expect(page.locator(".detail-inspector")).toHaveCount(0);
  await expectNoHorizontalOverflow(page.locator(".history-workspace"));

  const accessibility = await new AxeBuilder({ page })
    .include(".history-workspace")
    .analyze();
  expect(accessibility.violations).toEqual([]);
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
  const older = record(IDS.oldest, now - 25 * 60 * 60_000);
  await seedHistory(page, [older, recent]);
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
      .locator(`[data-history-id="${IDS.oldest}"]`)
      .locator(".date-cell strong"),
  ).not.toContainText("ago");

  await recentRow.click();
  const responsiveness = page
    .locator(".detail-group")
    .filter({ hasText: "Responsiveness" });
  await responsiveness.getByRole("button", { name: /Responsiveness/ }).click();
  await expect(responsiveness.locator(".stage-unavailable")).toContainText(
    "Not run",
  );
  await expect(responsiveness.locator(".metric-grid")).toHaveCount(0);
  await expect(responsiveness.locator(".loaded-row")).toHaveCount(0);
  await expect(responsiveness.locator(".bufferbloat-band")).toHaveCount(0);
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
  await expect(page.locator(".archive-toolbar")).not.toContainText("Clear");
  const clearAll = page.getByRole("button", {
    name: "Clear all saved results",
  });
  const rowBox = await page.locator(".result-row").boundingBox();
  const clearBox = await clearAll.boundingBox();
  expect(clearBox!.y).toBeGreaterThan(rowBox!.y + rowBox!.height);
  await clearAll.click();
  const clearDialog = page.getByRole("alertdialog", {
    name: "Clear result history?",
  });
  await expect(clearDialog).toBeVisible();
  await clearDialog.getByRole("button", { name: "Cancel" }).click();
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
  await page.getByRole("button", { name: "Close History" }).first().click();
  await expect(
    page.getByRole("button", { name: "Start the speed test" }),
  ).toBeVisible();
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
});

test("column visibility persists and narrow cards retain all six sort choices", async ({
  page,
}) => {
  await openApp(page, "dummy", { width: 1024, height: 768 });
  await seedHistory(page, [record(IDS.newest, Date.UTC(2026, 7, 28, 12))]);
  await openHistory(page);
  await page.getByRole("button", { name: "Columns" }).click();
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
  await page.getByRole("button", { name: "View & sort" }).click();
  await expect(page.locator(".view-popover").getByRole("radio")).toHaveCount(6);
  await expectNoHorizontalOverflow(page.locator(".view-popover"));
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
  const detailSurface = page.locator(".result-detail .phase-row").first();
  await detailSurface.hover();
  const beforeDetailWheel = await scrollOwner.evaluate(
    (node) => node.scrollTop,
  );
  const detailBox = await detailSurface.boundingBox();
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

  await detail.getByRole("button", { name: /Responsiveness/ }).click();
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

  const phase = detail.locator(".phase-row").first();
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
    page.getByRole("button", { name: "Clear all saved results" }),
  ).toHaveCount(0);

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
  await expect(
    page.getByRole("button", { name: "Clear all saved results" }),
  ).toHaveCount(0);
  await page.locator(".load-more button").evaluate((button) => button.click());
  await expect(page.locator(".result-row")).toHaveCount(120);
  await expect(
    page.getByRole("button", { name: "Clear all saved results" }),
  ).toBeVisible();
});
