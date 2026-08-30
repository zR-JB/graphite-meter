import { AxeBuilder } from "./webview";
import {
  configureSettings,
  expect,
  expectVisible,
  expectNoHorizontalOverflow,
  openApp,
  openEndpointInfo,
  openSettings,
  startTest,
  test,
} from "./webview";

type TestPage = Parameters<typeof openApp>[0];

async function openWithHistoryDefault(page: TestPage, enabled: boolean) {
  const index = await Bun.file(
    new URL("../dist/index.html", import.meta.url),
  ).text();
  await page.route("**/index.html*", (route) =>
    route.fulfill({
      body: index.replace(
        "<head>",
        `<head><meta name="graphite-meter-result-history-default" content="${enabled}">`,
      ),
      headers: { "content-type": "text/html; charset=utf-8" },
    }),
  );
  await page.goto("/index.html?engine=dummy");
}

async function seedRetainedResult(page: TestPage) {
  await page.evaluate(
    (saved) =>
      new Promise<void>((resolve, reject) => {
        const opening = indexedDB.open("graphite-meter", 1);
        opening.onupgradeneeded = () => {
          const results = opening.result.createObjectStore("results", {
            keyPath: "id",
          });
          results.createIndex("completedAt", "completedAt");
        };
        opening.onerror = () => reject(opening.error);
        opening.onsuccess = () => {
          const database = opening.result;
          const transaction = database.transaction("results", "readwrite");
          const results = transaction.objectStore("results");
          results.clear();
          results.put(saved);
          transaction.oncomplete = () => {
            database.close();
            resolve();
          };
          transaction.onerror = () => reject(transaction.error);
        };
      }),
    {
      schemaVersion: 1,
      id: "00000000-0000-4000-8000-000000000127",
      startedAt: 1,
      completedAt: 2,
      durationMs: 1,
      stages: {
        latency: {
          status: "not-run",
          result: null,
          lanes: {
            latency: null,
            download: null,
            upload: null,
            bidirectional: null,
          },
        },
        download: { status: "not-run", result: null },
        upload: { status: "not-run", result: null },
        bidirectional: { status: "not-run", down: null, up: null },
      },
      bufferbloat: null,
      totalBytes: 0,
      server: { name: "Reset test", location: null, engine: "dummy" },
      transport: {
        throughput: { protocol: null, kind: null },
        latency: { protocol: null, kind: null },
      },
      ipVersion: null,
      client: { build: "browser-test" },
      failures: [],
      wireEstimates: null,
    },
  );
}

async function takeRetainedResultCount(page: TestPage) {
  return page.evaluate(
    () =>
      new Promise<number>((resolve, reject) => {
        const opening = indexedDB.open("graphite-meter", 1);
        opening.onerror = () => reject(opening.error);
        opening.onsuccess = () => {
          const database = opening.result;
          const transaction = database.transaction("results", "readwrite");
          const results = transaction.objectStore("results");
          const request = results.count();
          transaction.onerror = () => reject(transaction.error);
          request.onerror = () => reject(request.error);
          request.onsuccess = () => {
            const count = request.result;
            results.clear();
            transaction.oncomplete = () => {
              database.close();
              resolve(count);
            };
          };
        };
      }),
  );
}
test("settings expose live controls and lock run construction inputs", async ({
  page,
}) => {
  await openApp(page);
  const settings = await openSettings(page);
  await expectVisible(settings.getByText("Ready", { exact: true }).first());
  await expectVisible(settings.getByLabel("Maximum H1 streams per direction"));
  const accessibility = await new AxeBuilder({ page })
    .include('[aria-label="Settings"]')
    .analyze();
  expect(accessibility.violations).toEqual([]);
  await startTest(page);
  await expectVisible(page.getByRole("button", { name: "Abort test" }));
  await expect(settings.getByRole("button", { name: "short" })).toBeEnabled();
  await expect(settings.getByLabel("Finish stable stages early")).toBeEnabled();
  await expect(settings.getByLabel("Unloaded ping cadence")).toBeDisabled();
  await expect(
    settings.getByLabel("Maximum H1 streams per direction"),
  ).toBeDisabled();
  await expect(
    settings.locator('input[name="throughput-target"]:checked'),
  ).toBeDisabled();
});

test("settings group result and advanced controls in the requested order", async ({
  page,
}) => {
  await openApp(page);
  const settings = await openSettings(page);
  const headings = settings.locator("h3");
  const order = await headings.evaluateAll((nodes) =>
    nodes.map((node) => node.textContent?.trim()),
  );
  expect(order.indexOf("Result history")).toBeLessThan(
    order.indexOf("Wire-rate estimates"),
  );
  expect(order.indexOf("Wire-rate estimates")).toBeLessThan(
    order.indexOf("Gauge scale"),
  );
  expect(order.indexOf("Transfer streams")).toBeGreaterThan(
    order.indexOf("Datagram throughput"),
  );
  await expect(settings.getByText("Minimum coverage")).toHaveCount(0);
  await expect(settings.getByText("Stability threshold")).toHaveCount(0);
  await expect(settings.getByText("Confirmation ms")).toHaveCount(0);
  await expect(
    settings.getByRole("link", { name: "View History" }),
  ).toBeVisible();
  await expect(
    settings.getByText(
      "Estimated Ethernet rate from measured protocol bytes and available connection details.",
    ),
  ).toBeVisible();
  await expect(
    settings.getByText(/conservative 1500 B Ethernet path/),
  ).toHaveCount(0);
});
test("connection paths stay single-column by default and reflow after a dock resize", async ({
  page,
}) => {
  await openApp(page, "dummy", { width: 1440, height: 900 });
  const settings = await openSettings(page);
  const columnCount = (locator: import("./webview").Locator) =>
    locator.evaluate((element) => {
      const columns = getComputedStyle(element).gridTemplateColumns.trim();
      return columns ? columns.split(/\s+/).length : 0;
    });
  const setupGrid = settings.locator(".setup-grid");
  const optionsGrid = settings.locator(".options").first();
  const setupColumns = () => columnCount(setupGrid);
  const optionColumns = () => columnCount(optionsGrid);
  expect(await optionColumns()).toBe(1);
  expect(await setupColumns()).toBe(1);
  const durationTops = await settings
    .locator(".dur-cell")
    .evaluateAll((cells) =>
      cells.map((cell) => cell.getBoundingClientRect().top),
    );
  expect(durationTops).toHaveLength(4);
  expect(
    Math.max(...durationTops) - Math.min(...durationTops),
  ).toBeLessThanOrEqual(1);
  await expectNoHorizontalOverflow(settings.locator(".panel-body"));
  const resize = page.getByRole("slider", {
    name: "Resize Settings panel (arrow keys; Enter to reset)",
  });
  let setupMultiColumnStep = -1;
  let optionsMultiColumnStep = -1;
  for (
    let step = 1;
    step <= 20 && (setupMultiColumnStep < 0 || optionsMultiColumnStep < 0);
    step++
  ) {
    await resize.press("ArrowRight");
    await page.waitForTimeout(50);
    if (setupMultiColumnStep < 0 && (await setupColumns()) > 1)
      setupMultiColumnStep = step;
    if (optionsMultiColumnStep < 0 && (await optionColumns()) > 1)
      optionsMultiColumnStep = step;
  }
  expect(setupMultiColumnStep).toBe(optionsMultiColumnStep);
  await expectNoHorizontalOverflow(settings.locator(".panel-body"));
});
test("endpoint cards own responsive details without panel overflow", async ({
  page,
}) => {
  await openApp(page, "dummy", { width: 1440, height: 900 });
  const endpoint = await openEndpointInfo(page);
  const grid = endpoint.locator(".grid");
  const firstRow = endpoint.locator(".card").first().locator("dl div").first();
  const columns = (locator: ReturnType<typeof endpoint.locator>) =>
    locator.evaluate((element) => {
      const value = getComputedStyle(element).gridTemplateColumns.trim();
      return value ? value.split(/\s+/).length : 0;
    });
  const resize = page.getByRole("slider", {
    name: "Resize Endpoint panel (arrow keys; Enter to reset)",
  });
  await resize.press("Shift+ArrowRight");
  await resize.press("Shift+ArrowRight");
  await expect.poll(() => columns(firstRow)).toBe(1);
  await expect.poll(() => columns(grid)).toBe(1);
  await expectNoHorizontalOverflow(endpoint.locator(".panel-body"));
  await resize.press("Enter");
  await expect.poll(() => columns(grid)).toBe(1);
  await expect.poll(() => columns(firstRow)).toBe(2);
  await expectNoHorizontalOverflow(endpoint);
  for (let step = 0; step < 4; step++) await resize.press("Shift+ArrowLeft");
  await expect.poll(() => columns(grid)).toBeGreaterThan(1);
  await expect(endpoint).toContainText(
    "Fetch streams · WebTransport streams · WebTransport datagrams",
  );
  await expectNoHorizontalOverflow(endpoint);
  const accessibility = await new AxeBuilder({ page })
    .include('[aria-label="Endpoint info"]')
    .analyze();
  expect(accessibility.violations).toEqual([]);
});
test("reset settings confirms, preserves on cancel, and restores defaults", async ({
  page,
}) => {
  await openApp(page, "dummy", { width: 1440, height: 900 });
  const settings = await openSettings(page);
  const panelWidth = await settings.evaluate(
    (element) => element.getBoundingClientRect().width,
  );
  await settings.getByRole("button", { name: "custom" }).click();
  await settings.getByLabel("Warmup ms").fill("1234");
  await settings.getByRole("button", { name: "Bytes", exact: true }).click();
  await settings.getByRole("button", { name: "Binary", exact: true }).click();
  await settings.getByText("Show estimated wire rate", { exact: true }).click();
  await settings.getByText("Force exact stream count", { exact: true }).click();
  await settings
    .getByText("Datagram throughput (experimental)", { exact: true })
    .click();
  const reset = settings.getByRole("button", { name: "Reset settings" });
  await reset.scrollIntoViewIfNeeded();
  await reset.focus();
  await reset.click();
  const dialog = page.getByRole("alertdialog");
  await expectVisible(dialog);
  await expectVisible(dialog.getByRole("heading", { name: "Reset settings?" }));
  const themeButton = page.getByRole("button", { name: /^Theme:/ });
  const themeBefore = await themeButton.getAttribute("aria-label");
  expect(themeBefore).not.toBeNull();
  const keepSettings = dialog.getByRole("button", { name: "Keep settings" });
  await expect(keepSettings).toBeFocused();
  await keepSettings.press("s");
  await keepSettings.press("t");
  await expectVisible(settings);
  await expectVisible(dialog);
  await expect(themeButton).toHaveAttribute("aria-label", themeBefore!);
  const accessibility = await new AxeBuilder({ page })
    .include('[aria-label="Settings"]')
    .include('[role="alertdialog"]')
    .analyze();
  expect(accessibility.violations).toEqual([]);
  await dialog.getByRole("button", { name: "Keep settings" }).click();
  await expect(reset).toBeFocused();
  await expect(settings.getByLabel("Warmup ms")).toHaveValue("1234");
  await reset.click();
  await page
    .getByRole("alertdialog")
    .getByRole("button", { name: "Reset settings" })
    .click();
  await expect(
    settings.getByRole("button", { name: "medium" }),
  ).toHaveAttribute("aria-pressed", "true");
  await expect(
    settings.getByLabel("Include concurrent download + upload"),
  ).not.toBeChecked();
  await expect(
    settings.getByLabel("Force exact stream count"),
  ).not.toBeChecked();
  await expect(
    settings.getByRole("button", { name: "Bits", exact: true }),
  ).toHaveAttribute("aria-pressed", "true");
  await expect(
    settings.getByRole("button", { name: "Decimal", exact: true }),
  ).toHaveAttribute("aria-pressed", "true");
  await expect(settings.getByLabel("Show estimated wire rate")).toBeChecked();
  await expect(
    settings.getByLabel("Datagram throughput (experimental)"),
  ).not.toBeChecked();
  await expect(
    settings.getByLabel("Scale throughput automatically"),
  ).toBeChecked();
  const finalWidth = await settings.evaluate(
    (element) => element.getBoundingClientRect().width,
  );
  expect(Math.abs(finalWidth - panelWidth)).toBeLessThanOrEqual(1);
  await startTest(page);
  await expect(
    settings.getByRole("button", { name: "Reset settings" }),
  ).toBeDisabled();
});

for (const operatorDefault of [false, true]) {
  test(`reset returns History to operator default ${operatorDefault} and retains results`, async ({
    page,
  }) => {
    await openWithHistoryDefault(page, operatorDefault);
    const settings = await openSettings(page);
    const historyToggle = settings.getByLabel(
      "Save completed results on this device",
    );
    if (operatorDefault) await expect(historyToggle).toBeChecked();
    else await expect(historyToggle).not.toBeChecked();

    await settings
      .getByText("Save completed results on this device", { exact: true })
      .click();
    if (operatorDefault) await expect(historyToggle).not.toBeChecked();
    else await expect(historyToggle).toBeChecked();
    await seedRetainedResult(page);

    const reset = settings.getByRole("button", { name: "Reset settings" });
    await reset.scrollIntoViewIfNeeded();
    await reset.click();
    await page
      .getByRole("alertdialog", { name: "Reset settings?" })
      .getByRole("button", { name: "Reset settings" })
      .click();

    if (operatorDefault) await expect(historyToggle).toBeChecked();
    else await expect(historyToggle).not.toBeChecked();
    await page.waitForTimeout(350);
    expect(
      await page.evaluate(() => {
        const raw = localStorage.getItem("graphite-meter:v1");
        return raw ? JSON.parse(raw).resultHistoryPreference : null;
      }),
    ).toBe("default");
    expect(await takeRetainedResultCount(page)).toBe(1);
    if (operatorDefault)
      await expect(
        page.getByRole("button", { name: "Open History" }),
      ).toBeVisible();
    else
      await expect(
        page.getByRole("button", { name: "Open History" }),
      ).toHaveCount(0);
  });
}
test("the datagram card follows its selection and announces its caution", async ({
  page,
}) => {
  await openApp(page);
  const settings = await openSettings(page);
  await expectVisible(settings.getByText("Ready", { exact: true }).first());
  const card = settings.locator("label", {
    hasText: "WebTransport datagrams",
  });
  const caution = settings
    .getByRole("status")
    .filter({ hasText: "not link speed" });
  const toggle = settings.getByText("Datagram throughput (experimental)");
  await expect(card).toHaveCount(0);
  await expect(caution).toHaveCount(0);
  await toggle.click();
  await expect(card).toHaveCount(1);
  await expect(caution).toContainText("Datagrams are never resent");
  await card.click();
  await expect(card.locator("input")).toBeChecked();
  await toggle.click();
  await expect(card.locator("input")).toBeChecked();
  await expect(caution).toBeVisible();
  const accessibility = await new AxeBuilder({ page })
    .include('[aria-label="Settings"]')
    .analyze();
  expect(accessibility.violations).toEqual([]);
});
test("endpoint summary and diagnostics use accessible disclosure", async ({
  page,
}) => {
  await openApp(page);
  const endpoint = await openEndpointInfo(page);
  await expectVisible(endpoint.getByText("Fetch stream · HTTP/1.1 · clear"));
  await expectVisible(
    endpoint.getByText(
      "Fetch streams · WebTransport streams · WebTransport datagrams",
    ),
  );
  await expectVisible(
    endpoint.getByText("WebSocket · WebTransport datagrams", { exact: true }),
  );
  await expectVisible(endpoint.getByText("Fetch stream over HTTP/1.1 · clear"));
  const summary = endpoint.locator("summary", { hasText: "Diagnostics" });
  await summary.focus();
  await summary.press("Enter");
  await expectVisible(endpoint.getByText("Server instance", { exact: true }));
  await expect(endpoint.locator(".diagnostic-note")).toContainText(
    "changes when the backend restarts",
  );
  await expectVisible(endpoint.getByText("Throughput origin"));
  await expectVisible(endpoint.getByText("Latency origin"));
  await expectVisible(
    endpoint.getByRole("button", { name: "Copy diagnostic report" }),
  );
  const accessibility = await new AxeBuilder({ page })
    .include('[aria-label="Endpoint info"]')
    .analyze();
  expect(accessibility.violations).toEqual([]);
});
test("endpoint status follows the live, running, and terminal path modes", async ({
  page,
}) => {
  await openApp(page);
  const settings = await configureSettings(page, "short-600");
  await settings.getByRole("button", { name: "Close Settings" }).click();
  const endpoint = await openEndpointInfo(page);
  const throughputPath = endpoint
    .locator(".path")
    .filter({ hasText: "throughput path" });
  await expect(throughputPath.locator("mark")).toHaveText("Ready");
  await startTest(page);
  await expect(throughputPath.locator("mark")).toHaveText("In use");
  const runSnapshot = await throughputPath.locator("dd").first().textContent();
  await expectVisible(
    page.getByRole("button", { name: "Run the test again" }),
    5_000,
  );
  await expect(throughputPath.locator("mark")).toHaveText("Used");
  await expect(throughputPath.locator("mark")).toHaveAttribute(
    "data-state",
    "used",
  );
  await expect(throughputPath.locator("dd").first()).toHaveText(runSnapshot!);
  const download = page.getByRole("switch", { name: "Download stage" });
  await download.click();
  await expect(throughputPath.locator("dd").first()).toHaveText(runSnapshot!);
});
