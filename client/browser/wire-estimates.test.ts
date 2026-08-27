import {
  expect,
  expectVisible,
  prepareApp,
  openSettings,
  resultCards,
  startAndWait,
  startTest,
  test,
  waitForCompletion,
} from "./webview";
test("default wire estimates stay out of live measurement and concise after completion", async ({
  page,
}) => {
  await prepareApp(page, "short");
  await expect(page.getByLabel("Show estimated wire rate")).toBeChecked();
  await startTest(page);
  await expect(page.locator(".gauge-value")).not.toHaveText("—");
  await expect(page.locator(".metric-wrap")).not.toContainText(/wire/i);
  await expect(page.locator(".result-chip")).not.toContainText(/wire/i);
  await expect(page.locator(".result-card .est")).toHaveCount(0);
  await waitForCompletion(page);
  const estimate = resultCards(page).locator(".est");
  await expect(estimate).toContainText("wire +");
  await expect(estimate).not.toContainText("wire estimate");
});
test("a persisted opt-out hides only wire-estimate presentation", async ({
  page,
}) => {
  await prepareApp(page, "short");
  await page.getByText("Show estimated wire rate", { exact: true }).click();
  await expect(page.getByLabel("Show estimated wire rate")).not.toBeChecked();
  await expect
    .poll(() =>
      page.evaluate(() => {
        const raw = localStorage.getItem("graphite-meter:v1");
        return raw ? JSON.parse(raw).showWireEstimates : null;
      }),
    )
    .toBe(false);
  await page.reload();
  await openSettings(page);
  await expect(page.getByLabel("Show estimated wire rate")).not.toBeChecked();
  await startTest(page);
  await expect(page.locator(".gauge-value")).not.toHaveText("—");
  await expect(page.locator(".result-card .est")).toHaveCount(0);
});
test("bidirectional results use their combined lane estimate", async ({
  page,
}) => {
  const settings = await prepareApp(page, "short");
  await settings
    .locator("label.switch", {
      hasText: "Include concurrent download + upload",
    })
    .click();
  await settings.getByLabel("Download ms").fill("0");
  await settings.getByLabel("Bidirectional ms").fill("900");
  await startAndWait(page);
  const card = resultCards(page).filter({ hasText: "Bi-dir" });
  await expect(card.locator(".est")).toContainText("wire +");
  await card.locator(".est-tag").hover();
  await expect(page.getByRole("tooltip")).toContainText("Total +");
});
test("result wire details work with mouse, keyboard, touch, and narrow viewports", async ({
  page,
}) => {
  await prepareApp(page, "short", "dummy", { width: 360, height: 740 });
  await startAndWait(page);
  const tag = resultCards(page).locator(".est-tag");
  await expect(tag).toHaveCSS("text-decoration-line", "underline");
  await expect(tag).toHaveCSS("text-decoration-style", "dotted");
  await tag.hover();
  const tooltip = page.getByRole("tooltip");
  await expect(tooltip).toContainText("Local Ethernet ·");
  await expect(tooltip).not.toContainText("(detected)");
  await expect(tooltip).toContainText(/IPv[46].*MTU/);
  await expect(tooltip).toContainText("Ethernet +");
  await expect(tooltip).toContainText("Total +");
  await expect(tooltip).toHaveCSS("white-space", "pre-line");
  const box = await tooltip.boundingBox();
  expect(box).not.toBeNull();
  expect(box!.x).toBeGreaterThanOrEqual(0);
  expect(box!.x + box!.width).toBeLessThanOrEqual(360);
  await page.mouse.move(0, 0);
  await expect(tooltip).toHaveCount(0);
  await tag.focus();
  await page.keyboard.press("Tab");
  await page.keyboard.press("Shift+Tab");
  await expectVisible(page.getByRole("tooltip"));
  await page.keyboard.press("Escape");
  await expect(page.getByRole("tooltip")).toHaveCount(0);
  await tag.dispatchEvent("pointerup", { pointerType: "touch" });
  await expectVisible(page.getByRole("tooltip"));
  await tag.dispatchEvent("pointerup", { pointerType: "touch" });
  await expect(page.getByRole("tooltip")).toHaveCount(0);
});
