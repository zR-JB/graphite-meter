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
import { gaugeLayout } from "../src/lib/canvas/gaugeLayout";
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
    expect(withLatency.height).toBeGreaterThanOrEqual(280);
    expect(withLatency.height).toBeLessThanOrEqual(320);
    expectNear(withLatency.canvasWidth, withoutLatency.canvasWidth);
    expectNear(withLatency.canvasHeight, withoutLatency.canvasHeight);
  });
}
async function assertGaugeLabels(page: Page) {
  const labels = page.locator(".gauge-tick");
  await expect(labels).toHaveCount(5);
  const stageSize = await page
    .locator(".gauge-panel .stage")
    .evaluate((element) => {
      const box = element.getBoundingClientRect();
      const style = getComputedStyle(element);
      return {
        width:
          box.width -
          Number.parseFloat(style.borderLeftWidth) -
          Number.parseFloat(style.borderRightWidth),
        height:
          box.height -
          Number.parseFloat(style.borderTopWidth) -
          Number.parseFloat(style.borderBottomWidth),
      };
    });
  const expected = gaugeLayout(stageSize.width, stageSize.height);
  const tickOuter = Math.hypot(
    expected.majorTicks[0]!.to.x - expected.center.x,
    expected.majorTicks[0]!.to.y - expected.center.y,
  );
  const anchors = await labels.evaluateAll((ticks) =>
    ticks.map((tick) => ({
      x: tick.getAttribute("data-anchor-x"),
      y: tick.getAttribute("data-anchor-y"),
    })),
  );
  expect(anchors).toEqual([
    { x: "end", y: "start" },
    { x: "end", y: "end" },
    { x: "center", y: "end" },
    { x: "start", y: "end" },
    { x: "start", y: "start" },
  ]);
  const boxes = await labels.evaluateAll((ticks) => {
    const stageElement = ticks[0]!.parentElement!.parentElement!;
    const stage = stageElement.getBoundingClientRect();
    const stageStyle = getComputedStyle(stageElement);
    const borderLeft = Number.parseFloat(stageStyle.borderLeftWidth);
    const borderTop = Number.parseFloat(stageStyle.borderTopWidth);
    const transform = new DOMMatrix(
      getComputedStyle(ticks[0]!.parentElement!).transform,
    );
    const center = {
      x: stage.width / 2 + transform.m41,
      y:
        stage.height / 2 +
        Number.parseFloat(
          stageStyle.getPropertyValue("--gauge-center-offset"),
        ) +
        transform.m42,
    };
    return ticks.map((tick) => {
      const box = tick.getBoundingClientRect();
      const style = getComputedStyle(tick);
      const anchor = {
        x: borderLeft + Number.parseFloat(style.left) + transform.m41,
        y: borderTop + Number.parseFloat(style.top) + transform.m42,
      };
      const left = box.left - stage.left;
      const right = box.right - stage.left;
      const top = box.top - stage.top;
      const bottom = box.bottom - stage.top;
      return {
        anchor,
        box: {
          left,
          right,
          top,
          bottom,
        },
        anchorX: tick.getAttribute("data-anchor-x"),
        anchorY: tick.getAttribute("data-anchor-y"),
        anchorRadius: Math.hypot(anchor.x - center.x, anchor.y - center.y),
        contained:
          box.left >= stage.left &&
          box.right <= stage.right &&
          box.top >= stage.top &&
          box.bottom <= stage.bottom,
      };
    });
  });
  for (const label of boxes) {
    if (label.anchorX === "end") expectNear(label.box.right, label.anchor.x);
    if (label.anchorX === "start") expectNear(label.box.left, label.anchor.x);
    if (label.anchorX === "center")
      expectNear((label.box.left + label.box.right) / 2, label.anchor.x);
    if (label.anchorY === "end") expectNear(label.box.bottom, label.anchor.y);
    if (label.anchorY === "start") expectNear(label.box.top, label.anchor.y);
    if (label.anchorY === "center")
      expectNear((label.box.top + label.box.bottom) / 2, label.anchor.y);
    // Allow opposite CSS-pixel rounding while rejecting material intrusion into the tick ring.
    expect(label.anchorRadius).toBeGreaterThanOrEqual(tickOuter - 1);
    expect(label.contained).toBe(true);
  }
  for (const [index, first] of boxes.entries()) {
    for (const second of boxes.slice(index + 1)) {
      expect(
        first.box.right <= second.box.left ||
          second.box.right <= first.box.left ||
          first.box.bottom <= second.box.top ||
          second.box.bottom <= first.box.top,
      ).toBe(true);
    }
  }
}

async function assertGaugeCanvasAlignment(page: Page) {
  const boxes = await page.locator(".gauge-panel .stage").evaluate((stage) => {
    const canvas = stage.querySelector("canvas");
    if (!(canvas instanceof HTMLCanvasElement))
      throw new Error("missing gauge");
    const stageBox = stage.getBoundingClientRect();
    const canvasBox = canvas.getBoundingClientRect();
    return {
      stage: {
        left: stageBox.left,
        right: stageBox.right,
        top: stageBox.top,
        bottom: stageBox.bottom,
        centerX: (stageBox.left + stageBox.right) / 2,
        centerY: (stageBox.top + stageBox.bottom) / 2,
      },
      canvas: {
        left: canvasBox.left,
        right: canvasBox.right,
        top: canvasBox.top,
        bottom: canvasBox.bottom,
        centerX: (canvasBox.left + canvasBox.right) / 2,
        centerY: (canvasBox.top + canvasBox.bottom) / 2,
      },
    };
  });
  expectNear(boxes.canvas.left, boxes.stage.left);
  expectNear(boxes.canvas.right, boxes.stage.right);
  expectNear(boxes.canvas.top, boxes.stage.top);
  expectNear(boxes.canvas.bottom, boxes.stage.bottom);
  expectNear(boxes.canvas.centerX, boxes.stage.centerX);
  expectNear(boxes.canvas.centerY, boxes.stage.centerY);
}

test("running and completed gauge geometry stays aligned to the shared layout", async ({
  page,
}) => {
  await openApp(page, "dummy", { width: 1024, height: 768 });
  const settings = await configureSettings(page, "three-stage");
  await settings.getByRole("button", { name: "Close Settings" }).click();
  await startTest(page);
  await expect(page.locator("#console")).toHaveAttribute(
    "data-phase",
    "download",
  );
  await assertGaugeCanvasAlignment(page);
  await assertGaugeLabels(page);
  await waitForCompletion(page, 10_000);
  await assertGaugeCanvasAlignment(page);
  await assertGaugeLabels(page);
});

for (const viewport of [
  { name: "desktop", width: 1440, height: 900 },
  { name: "mobile", width: 390, height: 844 },
]) {
  test(`throughput gauge labels stay anchored and separated at ${viewport.name} width`, async ({
    page,
  }) => {
    await openApp(page, "dummy", viewport);
    await startTest(page);
    await expect(page.locator("#console")).toHaveAttribute(
      "data-phase",
      "download",
    );
    await assertGaugeLabels(page);
  });
}
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
  await expect(page.locator(".metric-wrap .terminal-readout")).toHaveCount(1);
  await expect(
    page.locator(".terminal-readout.download .terminal-number"),
  ).toHaveText(
    await page.locator(".result-card:has(.ico.dl) .num").innerText(),
  );
  await expect(page.locator(".metric-wrap .terminal-unit")).toHaveText(
    /(?:bit|B)\/s$/,
  );
  await expect(page.locator(".gauge-panel output")).toContainText("Upload");
  const terminalAlignment = await page
    .locator(".terminal-readout")
    .evaluate((readout) => {
      const number = readout.querySelector(".terminal-number")!;
      const unit = readout.querySelector(".terminal-unit")!;
      const numberBox = number.getBoundingClientRect();
      const unitBox = unit.getBoundingClientRect();
      return {
        unitCenter: unitBox.left + unitBox.width / 2,
        numberCenter: numberBox.left + numberBox.width / 2,
      };
    });
  expect(
    Math.abs(terminalAlignment.unitCenter - terminalAlignment.numberCenter),
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
