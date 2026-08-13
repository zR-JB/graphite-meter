import AxeBuilder from "@axe-core/playwright";
import { expect, test } from "@playwright/test";

test("settings expose live controls and lock run construction inputs", async ({
  page,
}) => {
  await page.goto("/?engine=dummy");
  await page.getByRole("button", { name: "Open settings" }).click();
  const settings = page.locator('[aria-label="Settings"]');

  await expect(
    settings.getByText("Ready", { exact: true }).first(),
  ).toBeVisible();
  await expect(
    settings.getByLabel("Maximum H1 streams per direction"),
  ).toBeVisible();

  const advanced = settings.locator("summary", {
    hasText: "Customize the compensation model",
  });
  await advanced.focus();
  await advanced.press("Enter");
  await expect(settings.getByLabel("Connection profile")).toBeVisible();

  const accessibility = await new AxeBuilder({ page })
    .include('[aria-label="Settings"]')
    .analyze();
  expect(accessibility.violations).toEqual([]);

  await page.getByRole("button", { name: "Start the speed test" }).click();
  await expect(page.getByRole("button", { name: "Abort test" })).toBeVisible();
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

test("connection paths stay single-column by default and reflow after a dock resize", async ({
  page,
}) => {
  await page.setViewportSize({ width: 1440, height: 900 });
  await page.goto("/?engine=dummy");
  await page.getByRole("button", { name: "Open settings" }).click();

  const settings = page.locator('[aria-label="Settings"]');
  const columnCount = (locator: import("@playwright/test").Locator) =>
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
  await expect
    .poll(async () =>
      settings
        .locator(".panel-body")
        .evaluate((element) => element.scrollWidth <= element.clientWidth + 1),
    )
    .toBe(true);

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
  await expect
    .poll(async () =>
      settings
        .locator(".panel-body")
        .evaluate((element) => element.scrollWidth <= element.clientWidth + 1),
    )
    .toBe(true);
});

test("endpoint cards own responsive details without panel overflow", async ({
  page,
}) => {
  await page.setViewportSize({ width: 1440, height: 900 });
  await page.goto("/?engine=dummy");
  await page.getByRole("button", { name: "Toggle endpoint info" }).click();
  const endpoint = page.locator('[aria-label="Endpoint info"]');
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
  await expect
    .poll(async () =>
      endpoint
        .locator(".panel-body")
        .evaluate((element) => element.scrollWidth <= element.clientWidth + 1),
    )
    .toBe(true);

  await resize.press("Enter");
  await expect.poll(() => columns(grid)).toBe(1);
  await expect.poll(() => columns(firstRow)).toBe(2);
  await expect
    .poll(async () =>
      endpoint.evaluate(
        (element) => element.scrollWidth <= element.clientWidth + 1,
      ),
    )
    .toBe(true);

  for (let step = 0; step < 4; step++) await resize.press("Shift+ArrowLeft");
  await expect.poll(() => columns(grid)).toBeGreaterThan(1);
  await expect(endpoint).toContainText(
    "Fetch streams · WebTransport streams · WebTransport datagrams",
  );
  await expect
    .poll(async () =>
      endpoint.evaluate(
        (element) => element.scrollWidth <= element.clientWidth + 1,
      ),
    )
    .toBe(true);

  const accessibility = await new AxeBuilder({ page })
    .include('[aria-label="Endpoint info"]')
    .analyze();
  expect(accessibility.violations).toEqual([]);
});

test("reset settings confirms, preserves on cancel, and restores defaults", async ({
  page,
}) => {
  await page.setViewportSize({ width: 1440, height: 900 });
  await page.goto("/?engine=dummy");
  await page.getByRole("button", { name: "Open settings" }).click();
  const settings = page.locator('[aria-label="Settings"]');
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
  await reset.click();
  const dialog = page.getByRole("alertdialog");
  await expect(dialog).toBeVisible();
  await expect(
    dialog.getByRole("heading", { name: "Reset settings?" }),
  ).toBeVisible();
  const themeButton = page.getByRole("button", { name: /^Theme:/ });
  const themeBefore = await themeButton.getAttribute("aria-label");
  expect(themeBefore).not.toBeNull();
  const keepSettings = dialog.getByRole("button", { name: "Keep settings" });
  await expect(keepSettings).toBeFocused();
  await keepSettings.press("s");
  await keepSettings.press("t");
  await expect(settings).toBeVisible();
  await expect(dialog).toBeVisible();
  await expect(themeButton).toHaveAttribute("aria-label", themeBefore!);
  const accessibility = await new AxeBuilder({ page })
    .include('[aria-label="Settings"]')
    .include('[role="alertdialog"]')
    .analyze();
  expect(accessibility.violations).toEqual([]);

  await dialog.getByRole("button", { name: "Keep settings" }).click();
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

  await page.getByRole("button", { name: "Start the speed test" }).click();
  await expect(
    settings.getByRole("button", { name: "Reset settings" }),
  ).toBeDisabled();
});

// The datagram card is gated on its experimental setting, but a card already
// selected must not vanish under the user — and the note that its number is not
// a speed test belongs to that selection, not to the toggle. Read linearly the
// note is textually indistinguishable from the hints around it unless it
// announces itself, so a screen reader is told it appeared. Sighted readers get
// it above the toggle rather than below, where the end of the scroll hides it.
test("the datagram card follows its selection and announces its caution", async ({
  page,
}) => {
  await page.goto("/?engine=dummy");
  await page.getByRole("button", { name: "Open settings" }).click();
  const settings = page.locator('[aria-label="Settings"]');
  await expect(
    settings.getByText("Ready", { exact: true }).first(),
  ).toBeVisible();

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

  // Turning the setting off leaves the selection standing, so the card and its
  // caution both stay: a run is still about to happen over datagrams.
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
  await page.goto("/?engine=dummy");
  await page.getByRole("button", { name: "Toggle endpoint info" }).click();
  const endpoint = page.locator('[aria-label="Endpoint info"]');

  await expect(
    endpoint.getByText("Fetch stream · HTTP/1.1 · clear"),
  ).toBeVisible();
  await expect(
    endpoint.getByText(
      "Fetch streams · WebTransport streams · WebTransport datagrams",
    ),
  ).toBeVisible();
  await expect(endpoint.getByText("WebSocket", { exact: true })).toBeVisible();
  await expect(
    endpoint.getByText("Fetch stream over HTTP/1.1 · clear"),
  ).toBeVisible();
  const summary = endpoint.locator("summary", { hasText: "Diagnostics" });
  await summary.focus();
  await summary.press("Enter");
  await expect(
    endpoint.getByText("Server instance", { exact: true }),
  ).toBeVisible();
  await expect(endpoint.locator(".diagnostic-note")).toContainText(
    "changes when the backend restarts",
  );
  await expect(endpoint.getByText("Throughput origin")).toBeVisible();
  await expect(endpoint.getByText("Latency origin")).toBeVisible();
  await expect(
    endpoint.getByRole("button", { name: "Copy diagnostic report" }),
  ).toBeVisible();

  const accessibility = await new AxeBuilder({ page })
    .include('[aria-label="Endpoint info"]')
    .analyze();
  expect(accessibility.violations).toEqual([]);
});

test("endpoint status follows the live, running, and terminal path modes", async ({
  page,
}) => {
  await page.goto("/?engine=dummy");
  await page.getByRole("button", { name: "Open settings" }).click();
  const settings = page.locator('[aria-label="Settings"]');
  await settings.getByRole("button", { name: "custom" }).click();
  for (const [label, value] of [
    ["Warmup ms", "0"],
    ["Latency ms", "0"],
    ["Download ms", "600"],
    ["Upload ms", "0"],
  ] as const)
    await settings.getByLabel(label).fill(value);
  await settings.getByRole("button", { name: "Close Settings" }).click();

  await page.getByRole("button", { name: "Toggle endpoint info" }).click();
  const endpoint = page.locator('[aria-label="Endpoint info"]');
  const throughputPath = endpoint
    .locator(".path")
    .filter({ hasText: "throughput path" });
  await expect(throughputPath.locator("mark")).toHaveText("Ready");

  await page.getByRole("button", { name: "Start the speed test" }).click();
  await expect(throughputPath.locator("mark")).toHaveText("In use");
  const runSnapshot = await throughputPath.locator("dd").first().textContent();

  await expect(
    page.getByRole("button", { name: "Run the test again" }),
  ).toBeVisible({ timeout: 5_000 });
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
