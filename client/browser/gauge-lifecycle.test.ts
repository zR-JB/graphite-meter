import {
  abortButton,
  againButton,
  configureSettings,
  expect,
  gaugeStage,
  openApp,
  startTest,
  test,
  type Page,
} from "./webview";
interface GaugeSnapshot {
  stageWidth: number;
  stageHeight: number;
  canvasWidth: number;
  canvasHeight: number;
  backingWidth: number;
  backingHeight: number;
  expectedBackingWidth: number;
  expectedBackingHeight: number;
  opaquePixels: number;
}
async function snapshotGauge(page: Page): Promise<GaugeSnapshot> {
  return page.locator(".gauge-panel .stage").evaluate((stage) => {
    const canvas = stage.querySelector("canvas");
    if (!(canvas instanceof HTMLCanvasElement))
      throw new Error("missing gauge");
    const stageBox = stage.getBoundingClientRect();
    const canvasBox = canvas.getBoundingClientRect();
    const dpr = Math.min(window.devicePixelRatio || 1, 2);
    const pixels = canvas
      .getContext("2d")
      ?.getImageData(0, 0, canvas.width, canvas.height).data;
    let opaquePixels = 0;
    if (pixels)
      for (let index = 3; index < pixels.length; index += 4)
        if (pixels[index] > 0) opaquePixels++;
    return {
      stageWidth: stageBox.width,
      stageHeight: stageBox.height,
      canvasWidth: canvasBox.width,
      canvasHeight: canvasBox.height,
      backingWidth: canvas.width,
      backingHeight: canvas.height,
      expectedBackingWidth: Math.round(canvasBox.width * dpr),
      expectedBackingHeight: Math.round(canvasBox.height * dpr),
      opaquePixels,
    };
  });
}
async function expectCoherentGauge(page: Page): Promise<void> {
  await expect(gaugeStage(page).locator("canvas")).toBeVisible();
  let gauge!: GaugeSnapshot;
  // Resizing clears the backing store; the shared frame scheduler paints later.
  await expect
    .poll(async () => {
      gauge = await snapshotGauge(page);
      return (
        gauge.backingWidth === gauge.expectedBackingWidth &&
        gauge.backingHeight === gauge.expectedBackingHeight &&
        gauge.opaquePixels > 0
      );
    })
    .toBe(true);
  expect(Math.abs(gauge.stageWidth - gauge.canvasWidth)).toBeLessThanOrEqual(2);
  expect(Math.abs(gauge.stageHeight - gauge.canvasHeight)).toBeLessThanOrEqual(
    2,
  );
  expect(gauge.backingWidth).toBe(gauge.expectedBackingWidth);
  expect(gauge.backingHeight).toBe(gauge.expectedBackingHeight);
  expect(gauge.opaquePixels).toBeGreaterThan(0);
}
for (const viewport of [
  { width: 1024, height: 768, label: "desktop" },
  { width: 390, height: 640, label: "mobile" },
]) {
  test(`gauge backing store survives ${viewport.label} lifecycle churn`, async ({
    page,
  }) => {
    const errors: string[] = [];
    page.on("pageerror", (error) => errors.push(error.message));
    await openApp(page, "dummy", viewport);
    await expectCoherentGauge(page);
    await page.setViewportSize({
      width: viewport.width,
      height: viewport.height + 120,
    });
    await expectCoherentGauge(page);
    await gaugeStage(page).evaluate((stage) => {
      stage.setAttribute("data-test-hidden", "true");
      (stage as HTMLElement).style.display = "none";
    });
    await page.waitForTimeout(80);
    await gaugeStage(page).evaluate((stage) => {
      stage.removeAttribute("data-test-hidden");
      (stage as HTMLElement).style.removeProperty("display");
    });
    await expectCoherentGauge(page);
    const settings = await configureSettings(page, "lifecycle");
    await settings.getByRole("button", { name: "Close Settings" }).click();
    await startTest(page);
    await expect(abortButton(page)).toBeVisible();
    await expectCoherentGauge(page);
    await abortButton(page).click();
    await expect(againButton(page)).toBeVisible();
    await expectCoherentGauge(page);
    await againButton(page).click();
    await expect(abortButton(page)).toBeVisible();
    await expectCoherentGauge(page);
    expect(errors).toEqual([]);
  });
}
