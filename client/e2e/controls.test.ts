import {
  baseConfig,
  frankfurt,
  open,
  openSettings,
  phase,
  ready,
  runButton,
  savedResult,
  spawnPeer,
} from "./fleet";
import { expect, test, type Locator } from "./webview";

const checked = (locator: Locator) =>
  locator.evaluate((el: HTMLInputElement) => el.checked);

test("reset settings confirms, preserves on cancel and restores defaults", async (page) => {
  await open(page);
  const settings = await openSettings(page);
  await settings.getByRole("button", { name: "custom", exact: true }).click();
  const warmup = settings.getByRole("textbox", { name: "Warmup ms" });
  await warmup.fill("1234");
  await settings.getByRole("button", { name: "Bytes", exact: true }).click();
  const wireLabel = "Show estimated wire rate";
  const wire = settings.getByRole("checkbox", { name: wireLabel });
  await settings.locator("label.switch", { hasText: wireLabel }).click();
  expect(await checked(wire)).toBe(false);

  const reset = settings.getByRole("button", { name: "Reset settings" });
  await reset.click();
  const dialog = page.getByRole("alertdialog", { name: "Reset settings?" });
  const keep = dialog.getByRole("button", { name: "Keep settings" });
  await expect(keep).toBeFocused();
  await keep.click();
  await expect(dialog).toHaveCount(0);
  await expect(reset).toBeFocused();
  await expect(warmup).toHaveValue("1234");

  await reset.click();
  await dialog.getByRole("button", { name: "Reset settings" }).click();
  await expect(dialog).toHaveCount(0);
  const pressed = (name: string) =>
    expect(settings.getByRole("button", { name, exact: true })).toHaveAttribute(
      "aria-pressed",
      "true",
    );
  await pressed("medium");
  await pressed("Bits");
  await pressed("Decimal");
  expect(await checked(wire)).toBe(true);
});

test("Escape closes a settings confirmation; Back closes it with its panel", async (page) => {
  await open(page);
  const settings = await openSettings(page);
  const reset = settings.getByRole("button", { name: "Reset settings" });
  const dialog = page.getByRole("alertdialog", { name: "Reset settings?" });
  await reset.click();
  await expect(dialog).toBeVisible();
  await page.raw.press("Escape");
  await expect(dialog).toHaveCount(0);
  await expect(reset).toBeFocused();

  await reset.click();
  await expect(dialog).toBeVisible();
  await page.evaluate(() => history.back());
  await expect(dialog).toHaveCount(0);
  await expect
    .poll(() =>
      settings.all((els: HTMLElement[]) => els.every((el) => el.inert)),
    )
    .toBe(true);
  await expect(
    page.getByRole("button", { name: "Open settings" }),
  ).toBeFocused();
});

test("legal notices recover through Retry and keep focus in the dialog", async (page) => {
  await open(page);
  await page.blockRequests(["*legal/about.json*"]);
  await page.getByRole("button", { name: "Details" }).click();
  await page.getByRole("button", { name: "About & legal" }).click();
  const dialog = page.getByRole("dialog", { name: "About & legal" });
  await expect(dialog).toContainText("Unable to load legal notices.");
  await page.cdp("Network.setBlockedURLs", { urls: [] });
  await dialog.getByRole("button", { name: "Retry" }).click();
  await expect(dialog).toContainText("Third-party software");

  const summary = dialog.locator(".component:last-child summary");
  const link = dialog.locator(".component:last-child a");
  const close = dialog.getByRole("button", { name: "Close", exact: true });
  await summary.evaluate((el: HTMLElement) => el.focus());
  await page.raw.press("Tab");
  await expect(link).toBeFocused();
  // Past the last control a modal hands focus to the browser, never the page.
  await page.raw.press("Tab");
  expect(await page.evaluate(() => document.activeElement?.tagName)).toBe(
    "BODY",
  );
  await page.raw.press("Tab");
  await expect(close).toBeFocused();
});

test("a History chunk that fails to load settles and recovers through Retry", async (page) => {
  await open(page);
  await page.blockRequests(["*HistoryWorkspace*"]);
  await page.getByRole("button", { name: "Open History" }).click();
  const stage = page.locator(".history-stage");
  await expect(stage).toContainText("History could not be opened.");
  const mutations = await page.evaluate(
    () =>
      new Promise((done) => {
        let count = 0;
        const observer = new MutationObserver(
          (records) => (count += records.length),
        );
        observer.observe(document.querySelector(".history-stage")!, {
          childList: true,
          subtree: true,
        });
        requestAnimationFrame(() =>
          requestAnimationFrame(() => {
            observer.disconnect();
            done(count);
          }),
        );
      }),
  );
  expect(mutations).toBe(0);
  await page.cdp("Network.setBlockedURLs", { urls: [] });
  await stage.getByRole("button", { name: "Retry" }).click();
  await expect(page.locator(".history-workspace")).toHaveCount(1);
});

test("the topbar menu opens, moves, acts and closes from the keyboard", async (page) => {
  await page.setViewportSize({ width: 300, height: 700 });
  await open(page);
  const trigger = page.getByRole("button", { name: "More controls" });
  const menu = page.getByRole("menu", { name: "More controls" });
  const items = menu.getByRole("menuitem");
  await trigger.evaluate((el: HTMLElement) => el.focus());
  await page.raw.press("ArrowDown");
  await expect(items.nth(0)).toBeFocused();
  await page.raw.press("ArrowDown");
  await expect(items.nth(1)).toBeFocused();
  await page.raw.press("Escape");
  await expect(menu).toHaveCount(0);
  await expect(trigger).toBeFocused();

  await page.raw.press("ArrowUp");
  await expect(items.nth(1)).toContainText("Details");
  await expect(items.nth(1)).toBeFocused();
  await page.raw.press("Enter");
  await expect(menu).toHaveCount(0);
  await expect
    .poll(() =>
      page
        .locator('.panel[aria-label="Details"]')
        .all((els: HTMLElement[]) => els.some((el) => !el.inert)),
    )
    .toBe(true);
});

test("the first result saves after the application server becomes unreachable", async (page) => {
  const { id, name, url } = frankfurt;
  const oslo = await spawnPeer("Oslo", {
    GM_SERVER_CATALOG: JSON.stringify({ servers: [{ id, name, url }] }),
  });
  try {
    await open(page, oslo.server.url, {
      servers: [frankfurt],
      latency: { mode: "primary", serverId: id },
      config: {
        stages: { ...baseConfig.stages, latency: false, upload: false },
        skipLoadedLatencyWhenStageOff: true,
        duration: { ...baseConfig.duration, downloadMs: 1500 },
      },
    });
    await ready(page);
    const startedAt = Date.now();
    await runButton(page, "Start test").click();
    await expect(phase(page, "download")).toHaveCount(1, { timeout: 10_000 });
    oslo.kill("SIGKILL");
    const saved = await savedResult(page, startedAt, 20_000);
    expect(saved.stages.download.status).toBe("complete");
  } finally {
    oslo.kill();
  }
});
