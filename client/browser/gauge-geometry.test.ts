import {
  configureSettings,
  expect,
  expectNear,
  expectVisible,
  openApp,
  openSettings,
  resultCards,
  startTest,
  test,
  waitForCompletion,
  type Page,
} from "./webview";
function persistedConfig(latency: boolean) {
  return JSON.stringify({
    config: {
      stages: {
        latency,
        download: true,
        upload: true,
        bidirectional: false,
      },
    },
  });
}
async function gaugeBox(page: Page, latency: boolean) {
  await page.evaluate(
    (value) => localStorage.setItem("graphite-meter:v1", value),
    persistedConfig(latency),
  );
  await page.reload();
  const stage = page.locator(".gauge-panel .stage");
  await expectVisible(stage);
  return stage.evaluate((element) => {
    const box = element.getBoundingClientRect();
    const canvas = element.querySelector("canvas");
    return {
      width: box.width,
      height: box.height,
      canvasWidth: canvas?.getBoundingClientRect().width ?? 0,
      canvasHeight: canvas?.getBoundingClientRect().height ?? 0,
    };
  });
}
for (const viewport of [
  { width: 390, height: 640 },
  { width: 390, height: 844 },
]) {
  test(`mobile gauge geometry is invariant with latency at ${viewport.height}px`, async ({
    page,
  }) => {
    await openApp(page, "dummy", viewport);
    const withLatency = await gaugeBox(page, true);
    const withoutLatency = await gaugeBox(page, false);
    expectNear(withLatency.width, withoutLatency.width);
    expectNear(withLatency.height, withoutLatency.height);
    expectNear(withLatency.canvasWidth, withoutLatency.canvasWidth);
    expectNear(withLatency.canvasHeight, withoutLatency.canvasHeight);
  });
}
test("gauge tick labels use shared optical anchors", async ({ page }) => {
  await openApp(page);
  await configureSettings(page, "latency-only");
  await startTest(page);
  await expect(page.locator("#console")).toHaveAttribute(
    "data-phase",
    "latency",
  );
  const anchors = await page.locator(".gauge-tick").evaluateAll((ticks) =>
    ticks.map((tick) => ({
      x: tick.getAttribute("data-anchor-x"),
      y: tick.getAttribute("data-anchor-y"),
    })),
  );
  expect(anchors).toEqual([
    { x: "start", y: "start" },
    { x: "start", y: "end" },
    { x: "center", y: "end" },
    { x: "end", y: "end" },
    { x: "end", y: "start" },
  ]);
});
test("a short landscape phone keeps anchored chrome and scrollable flyouts", async ({
  page,
}) => {
  await openApp(page, "dummy", { width: 844, height: 390 });
  const scrollability = await page.evaluate(() => ({
    viewportWidth: window.innerWidth,
    contentWidth: document.documentElement.scrollWidth,
    documentOverflow: getComputedStyle(document.documentElement).overflowY,
    stageClientHeight:
      document.querySelector("#console > .stage")?.clientHeight,
    stageScrollHeight:
      document.querySelector("#console > .stage")?.scrollHeight,
    gaugeWidth: document
      .querySelector(".gauge-panel .stage")
      ?.getBoundingClientRect().width,
    latencyWidth: document
      .querySelector(".gauge-panel .latency-panel")
      ?.getBoundingClientRect().width,
    chartWidth: document.querySelector(".chart")?.getBoundingClientRect().width,
  }));
  expect(scrollability.documentOverflow).toBe("clip");
  expect(scrollability.stageScrollHeight).toBeGreaterThan(
    scrollability.stageClientHeight!,
  );
  expect(scrollability.gaugeWidth).toBeGreaterThan(300);
  expect(scrollability.latencyWidth).toBeGreaterThan(300);
  expect(scrollability.chartWidth).toBeGreaterThan(700);
  expect(scrollability.contentWidth).toBeLessThanOrEqual(
    scrollability.viewportWidth + 1,
  );
  const panel = await openSettings(page);
  const panelSurface = await panel.evaluate((element) => {
    const box = element.getBoundingClientRect();
    const body = element.querySelector(".panel-body");
    const status = document.querySelector(".status")?.getBoundingClientRect();
    return {
      width: box.width,
      top: box.top,
      bottom: box.bottom,
      statusTop: status?.top,
      sheetHandle: getComputedStyle(element.querySelector(".sheet-handle")!)
        .display,
      bodyOverflow: body ? getComputedStyle(body).overflowY : null,
    };
  });
  expect(panelSurface.width).toBeLessThan(844);
  expect(panelSurface.top).toBeGreaterThanOrEqual(59);
  expect(panelSurface.bottom).toBeLessThanOrEqual(panelSurface.statusTop! + 1);
  expect(panelSurface.sheetHandle).toBe("none");
  expect(panelSurface.bodyOverflow).toBe("auto");
});
test("a portrait tablet keeps settings in a contained side flyout", async ({
  page,
}) => {
  await openApp(page, "dummy", { width: 768, height: 1024 });
  await openSettings(page);
  const panelSurface = await page
    .locator('[aria-label="Settings"]')
    .evaluate((element) => {
      const box = element.getBoundingClientRect();
      const status = document.querySelector(".status")?.getBoundingClientRect();
      return {
        width: box.width,
        top: box.top,
        bottom: box.bottom,
        statusTop: status?.top,
        sheetHandle: getComputedStyle(element.querySelector(".sheet-handle")!)
          .display,
      };
    });
  expect(panelSurface.width).toBeLessThan(768);
  expect(panelSurface.top).toBeGreaterThanOrEqual(59);
  expect(panelSurface.bottom).toBeLessThanOrEqual(panelSurface.statusTop! + 1);
  expect(panelSurface.sheetHandle).toBe("none");
});
test("an open phone panel changes from sheet to side flyout on rotation", async ({
  page,
}) => {
  await openApp(page, "dummy", { width: 390, height: 844 });
  const panel = await openSettings(page);
  await expectVisible(panel.locator(".sheet-handle"));
  await page.setViewportSize({ width: 844, height: 390 });
  await expect(panel.locator(".sheet-handle")).toBeHidden();
  const surface = () =>
    panel.evaluate((element) => {
      const box = element.getBoundingClientRect();
      const status = document.querySelector(".status")?.getBoundingClientRect();
      return {
        width: box.width,
        top: box.top,
        bottom: box.bottom,
        statusTop: status?.top,
        bodyOverflow: getComputedStyle(element.querySelector(".panel-body")!)
          .overflowY,
      };
    });
  await expect
    .poll(async () => {
      const current = await surface();
      return current.bottom - current.statusTop!;
    })
    .toBeLessThanOrEqual(1);
  const rotated = await surface();
  expect(rotated.width).toBeLessThan(844);
  expect(rotated.top).toBeGreaterThanOrEqual(59);
  expect(rotated.bottom).toBeLessThanOrEqual(rotated.statusTop! + 1);
  expect(rotated.bodyOverflow).toBe("auto");
});
async function configureThreeStageRun(page: Page) {
  const settings = await configureSettings(page, "three-stage");
  await settings.getByRole("button", { name: "Close Settings" }).click();
}
async function expectStageFits(page: Page) {
  const stage = await page
    .locator("#console > section.stage")
    .evaluate((element) => ({
      clientHeight: element.clientHeight,
      scrollHeight: element.scrollHeight,
    }));
  expect(stage.scrollHeight).toBeLessThanOrEqual(stage.clientHeight + 1);
}
test("a windowed desktop fits compact and final cards without token scrolling", async ({
  page,
}) => {
  await openApp(page, "dummy", { width: 1024, height: 768 });
  await configureThreeStageRun(page);
  await startTest(page);
  await expect(page.locator(".result-chip")).toHaveCount(3, {
    timeout: 10_000,
  });
  await expectStageFits(page);
  await waitForCompletion(page, 10_000);
  await expect(resultCards(page)).toHaveCount(3);
  await expect(page.locator(".metric-wrap .gauge-value")).toHaveCount(0);
  const terminalResults = page.locator(".metric-wrap .terminal-result");
  await expect(terminalResults).toHaveCount(2);
  await expect(
    page.locator(".terminal-result.download .terminal-number"),
  ).toHaveText(
    await page.locator(".result-card:has(.ico.dl) .num").innerText(),
  );
  await expect(
    page.locator(".terminal-result.upload .terminal-number"),
  ).toHaveText(
    await page.locator(".result-card:has(.ico.ul) .num").innerText(),
  );
  await expect(page.locator(".terminal-result.bidirectional")).toHaveCount(0);
  await expect(page.locator(".metric-wrap .terminal-unit")).toHaveText(
    /(?:bit|B)\/s$/,
  );
  const terminalAlignment = await page
    .locator(".terminal-unit")
    .evaluate((unit) => {
      const number = unit.parentElement?.querySelector(
        ".terminal-result.download .terminal-number",
      );
      if (!(number instanceof HTMLElement))
        throw new Error("missing terminal number");
      const unitBox = unit.getBoundingClientRect();
      const numberBox = number.getBoundingClientRect();
      return { unitRight: unitBox.right, numberRight: numberBox.right };
    });
  expect(
    Math.abs(terminalAlignment.unitRight - terminalAlignment.numberRight),
  ).toBeLessThanOrEqual(1);
  await expectStageFits(page);
});
for (const viewport of [
  { width: 1024, height: 640 },
  { width: 1024, height: 700 },
  { width: 1024, height: 768 },
  { width: 1200, height: 800 },
  { width: 1366, height: 768 },
  { width: 1440, height: 900 },
]) {
  test(`desktop gauge fits the stage at ${viewport.width}x${viewport.height}`, async ({
    page,
  }) => {
    await openApp(page, "dummy", viewport);
    const withLatency = await gaugeBox(page, true);
    const withoutLatency = await gaugeBox(page, false);
    expect(withoutLatency.width).toBeGreaterThan(withLatency.width * 1.5);
    expectNear(withLatency.height, withoutLatency.height);
    expectNear(withLatency.canvasHeight, withoutLatency.canvasHeight);
    const stage = await page
      .locator("#console > section.stage")
      .evaluate((element) => ({
        clientHeight: element.clientHeight,
        scrollHeight: element.scrollHeight,
      }));
    if (viewport.height === 640)
      expect(stage.scrollHeight).toBeGreaterThan(stage.clientHeight);
    else expect(stage.scrollHeight).toBeLessThanOrEqual(stage.clientHeight + 1);
    const chartContainment = await page.locator(".chart").evaluate((chart) => {
      const plot = chart.querySelector(".plot");
      if (!(plot instanceof HTMLElement)) throw new Error("missing chart plot");
      const chartBox = chart.getBoundingClientRect();
      const plotBox = plot.getBoundingClientRect();
      return {
        plotTop: plotBox.top - chartBox.top,
        plotBottom: plotBox.bottom - chartBox.bottom,
      };
    });
    expect(chartContainment.plotTop).toBeGreaterThanOrEqual(-1);
    expect(chartContainment.plotBottom).toBeLessThanOrEqual(1);
  });
}
