import { expect, test, type Page } from "./webview";

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
  await expect(page.locator(".gauge-panel .stage canvas")).toBeVisible();
  await page.waitForTimeout(80);
  const gauge = await snapshotGauge(page);
  expect(Math.abs(gauge.stageWidth - gauge.canvasWidth)).toBeLessThanOrEqual(2);
  expect(Math.abs(gauge.stageHeight - gauge.canvasHeight)).toBeLessThanOrEqual(
    2,
  );
  expect(gauge.backingWidth).toBe(gauge.expectedBackingWidth);
  expect(gauge.backingHeight).toBe(gauge.expectedBackingHeight);
  expect(gauge.opaquePixels).toBeGreaterThan(0);
}

async function configureShortRun(page: Page): Promise<void> {
  await page.getByRole("button", { name: "Open settings" }).click();
  const settings = page.locator('[aria-label="Settings"]');
  await settings.getByRole("button", { name: "custom" }).click();
  for (const [label, value] of [
    ["Warmup ms", "0"],
    ["Latency ms", "0"],
    ["Download ms", "1800"],
    ["Upload ms", "0"],
  ] as const)
    await settings.getByLabel(label).fill(value);
  await settings.getByRole("button", { name: "Close Settings" }).click();
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
    await page.setViewportSize(viewport);
    await page.goto("/?engine=dummy");
    await expectCoherentGauge(page);

    await page.setViewportSize({
      width: viewport.width,
      height: viewport.height + 120,
    });
    await expectCoherentGauge(page);

    await page.locator(".gauge-panel .stage").evaluate((stage) => {
      stage.setAttribute("data-test-hidden", "true");
      (stage as HTMLElement).style.display = "none";
    });
    await page.waitForTimeout(80);
    await page.locator(".gauge-panel .stage").evaluate((stage) => {
      stage.removeAttribute("data-test-hidden");
      (stage as HTMLElement).style.removeProperty("display");
    });
    await expectCoherentGauge(page);

    await configureShortRun(page);
    await page.getByRole("button", { name: "Start the speed test" }).click();
    await expect(
      page.getByRole("button", { name: "Abort test" }),
    ).toBeVisible();
    await expectCoherentGauge(page);

    await page.getByRole("button", { name: "Abort test" }).click();
    await expect(
      page.getByRole("button", { name: "Run the test again" }),
    ).toBeVisible();
    await expectCoherentGauge(page);

    await page.getByRole("button", { name: "Run the test again" }).click();
    await expect(
      page.getByRole("button", { name: "Abort test" }),
    ).toBeVisible();
    await expectCoherentGauge(page);
    expect(errors).toEqual([]);
  });
}
