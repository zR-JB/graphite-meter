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
    const dial = element.querySelector(".gauge-dial");
    return {
      width: box.width,
      height: box.height,
      dialWidth: dial?.getBoundingClientRect().width ?? 0,
      dialHeight: dial?.getBoundingClientRect().height ?? 0,
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
    expectNear(withLatency.dialWidth, withoutLatency.dialWidth);
    expectNear(withLatency.dialHeight, withoutLatency.dialHeight);
  });
}
async function assertGaugeLabels(page: Page) {
  const layout = () =>
    page.locator(".gauge-panel .stage").evaluate((stage) => {
      const box = stage.getBoundingClientRect();
      const labels = [...stage.querySelectorAll(".gauge-tick")].map((tick) =>
        tick.getBoundingClientRect(),
      );
      const ticks = [
        ...stage.querySelectorAll(
          '.gauge-dial g[stroke="var(--border-strong)"] path',
        ),
      ].map((tick) => tick.getBoundingClientRect());
      const separate = (a: DOMRect, b: DOMRect) =>
        a.right <= b.left ||
        b.right <= a.left ||
        a.bottom <= b.top ||
        b.bottom <= a.top;
      return {
        count: labels.length,
        contained: labels.every(
          (label) =>
            label.left >= box.left &&
            label.right <= box.right &&
            label.top >= box.top &&
            label.bottom <= box.bottom,
        ),
        separated: labels.every((label, index) =>
          labels.slice(index + 1).every((other) => separate(label, other)),
        ),
        clearOfTicks: labels.every((label) =>
          ticks.every((tick) => separate(label, tick)),
        ),
        ticks: ticks.length,
      };
    });
  await expect.poll(layout).toEqual({
    count: 5,
    contained: true,
    separated: true,
    clearOfTicks: true,
    ticks: 9,
  });
}

test("live and completed instruments keep labels and readouts contained", async ({
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
  for (const viewport of [
    { width: 1024, height: 768 },
    { width: 1440, height: 900 },
    { width: 390, height: 844 },
  ]) {
    await page.setViewportSize(viewport);
    await assertGaugeLabels(page);
  }
  await page.setViewportSize({ width: 1024, height: 768 });
  await expect(page.locator(".result-chip")).toHaveCount(3, {
    timeout: 10_000,
  });
  await expectStageFits(page);
  await waitForCompletion(page, 10_000);
  await assertGaugeLabels(page);
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
async function expectStageFits(page: Page) {
  const stage = await page
    .locator("#console > section.stage")
    .evaluate((element) => ({
      clientHeight: element.clientHeight,
      scrollHeight: element.scrollHeight,
    }));
  expect(stage.scrollHeight).toBeLessThanOrEqual(stage.clientHeight + 1);
}
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
    expectNear(withLatency.dialHeight, withoutLatency.dialHeight);
    const stage = await page
      .locator("#console > section.stage")
      .evaluate((element) => ({
        clientHeight: element.clientHeight,
        scrollHeight: element.scrollHeight,
      }));
    expect(stage.scrollHeight).toBeLessThanOrEqual(stage.clientHeight + 1);
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
