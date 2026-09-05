import {
  abortButton,
  expect,
  expectVisible,
  openApp,
  openSettings,
  prepareApp,
  startTest,
  test,
  waitForCompletion,
  type Page,
} from "./webview";
type Metrics = {
  __canvasDraws: number;
  __chartFrames: number;
  __frameWork: number[];
  __longTasks: number[];
};
const installPerformanceMetrics = async (page: Page) => {
  await page.addInitScript(() => {
    const metrics = window as unknown as Metrics;
    metrics.__canvasDraws = 0;
    metrics.__chartFrames = 0;
    metrics.__frameWork = [];
    metrics.__longTasks = [];
    const clearRect = CanvasRenderingContext2D.prototype.clearRect;
    CanvasRenderingContext2D.prototype.clearRect = function (...args) {
      metrics.__canvasDraws++;
      const canvas = this.canvas;
      if (
        canvas instanceof HTMLCanvasElement &&
        canvas.parentElement?.getAttribute("aria-label") ===
          "Throughput and latency over time"
      )
        metrics.__chartFrames++;
      return clearRect.apply(this, args);
    };
    const requestFrame = window.requestAnimationFrame.bind(window);
    window.requestAnimationFrame = (callback) =>
      requestFrame((now) => {
        const started = performance.now();
        callback(now);
        metrics.__frameWork.push(performance.now() - started);
      });
    try {
      new PerformanceObserver((list) => {
        for (const entry of list.getEntries())
          metrics.__longTasks.push(entry.duration);
      }).observe({ type: "longtask", buffered: true });
    } catch {
      // Older engines may not expose the Long Tasks API.
    }
  });
};
const performanceTest = (name: string, run: Parameters<typeof test>[1]) =>
  test(name, async (fixtures) => {
    await installPerformanceMetrics(fixtures.page);
    await run(fixtures);
  });
const draws = (page: Page) =>
  page.evaluate(() => (window as unknown as Metrics).__canvasDraws);
const chartSample = (page: Page) =>
  page.evaluate(() => ({
    frames: (window as unknown as Metrics).__chartFrames,
    now: performance.now(),
  }));
const resetMetrics = (page: Page) =>
  page.evaluate(() => {
    const metrics = window as unknown as Metrics;
    metrics.__frameWork.length = 0;
    metrics.__longTasks.length = 0;
  });
const performanceMetrics = (page: Page) =>
  page.evaluate(() => {
    const metrics = window as unknown as Metrics;
    const sorted = [...metrics.__frameWork].sort((a, b) => a - b);
    return {
      frameP95: sorted[Math.floor(sorted.length * 0.95)] ?? 0,
      longestTask: Math.max(0, ...metrics.__longTasks),
    };
  });
performanceTest(
  "canvas work parks when settled or offscreen",
  async ({ page, browserName, context }) => {
    if (browserName === "chromium") {
      const session = await context.newCDPSession(page);
      await session.send("Emulation.setCPUThrottlingRate", { rate: 4 });
    }
    await openApp(page);
    await expect
      .poll(async () => {
        const before = await draws(page);
        await page.waitForTimeout(500);
        return (await draws(page)) === before;
      })
      .toBe(true);
    await resetMetrics(page);
    await startTest(page);
    await page.waitForTimeout(1200);
    const plot = page.locator(
      '[role="slider"][aria-label="Throughput and latency over time"]',
    );
    await plot.scrollIntoViewIfNeeded();
    const box = await plot.boundingBox();
    if (!box) throw new Error("chart is not visible");
    const active = await chartSample(page);
    for (let i = 0; i < 40; i++)
      await page.mouse.move(
        box.x + (box.width * i) / 40,
        box.y + box.height / 2,
      );
    await page.waitForTimeout(1000);
    const afterPointer = await chartSample(page);
    const frameBudget = Math.ceil(
      ((afterPointer.now - active.now) * 30) / 1000,
    );
    expect(afterPointer.frames - active.frames).toBeLessThanOrEqual(
      frameBudget + 2,
    );
    await page.evaluate(() => {
      Object.defineProperty(document, "hidden", {
        value: true,
        configurable: true,
      });
      document.dispatchEvent(new Event("visibilitychange"));
    });
    await page.waitForTimeout(100);
    const hidden = await draws(page);
    await page.waitForTimeout(500);
    expect(await draws(page)).toBe(hidden);
    await page.evaluate(() => {
      Object.defineProperty(document, "hidden", {
        value: false,
        configurable: true,
      });
      document.dispatchEvent(new Event("visibilitychange"));
    });
    await page.waitForTimeout(100);
    expect(await draws(page)).toBeGreaterThan(hidden);
    await page.locator("canvas").evaluateAll((canvases) => {
      for (const canvas of canvases) canvas.style.display = "none";
    });
    await page.waitForTimeout(250);
    const offscreen = await draws(page);
    await page.waitForTimeout(500);
    expect(await draws(page)).toBe(offscreen);
    await page.locator("canvas").evaluateAll((canvases) => {
      for (const canvas of canvases) canvas.style.display = "";
    });
    await page.waitForTimeout(250);
    expect(await draws(page)).toBeGreaterThan(offscreen);
    await abortButton(page).click();
    await page.waitForTimeout(1800);
    const aborted = await draws(page);
    await page.waitForTimeout(500);
    expect(await draws(page)).toBe(aborted);
    const metrics = await performanceMetrics(page);
    expect(metrics.frameP95).toBeLessThan(50);
    // Chromium reports long tasks above 50 ms; under 4x CPU throttle, 100 ms still rejects presentation stalls.
    expect(metrics.longestTask).toBeLessThanOrEqual(100);
  },
);
performanceTest(
  "completed reduced-motion views settle after theme and resize",
  async ({ page }) => {
    await page.emulateMedia({ reducedMotion: "reduce" });
    await prepareApp(page, "active-presentation");
    await startTest(page);
    await waitForCompletion(page);
    await page.waitForTimeout(500);
    const complete = await draws(page);
    await page.waitForTimeout(500);
    expect(await draws(page)).toBe(complete);
    await page.getByRole("button", { name: /Theme: .*Click to cycle/ }).click();
    await page.setViewportSize({ width: 900, height: 700 });
    await page.waitForTimeout(250);
    expect(await draws(page)).toBeGreaterThan(complete);
    await page.waitForTimeout(500);
    const changed = await draws(page);
    await page.waitForTimeout(500);
    expect(await draws(page)).toBe(changed);
  },
);
performanceTest(
  "settings retains native scrolling during active presentation",
  async ({ page }) => {
    await openApp(page, "dummy", { width: 390, height: 640 });
    await startTest(page);
    await page.waitForTimeout(500);
    await openSettings(page);
    const body = page.locator('[aria-label="Settings"] .panel-body');
    await expectVisible(body);
    expect(
      await body.evaluate((element) => {
        const style = getComputedStyle(element);
        return (
          style.overflowY === "auto" &&
          element.scrollHeight > element.clientHeight
        );
      }),
    ).toBe(true);
    const box = await body.boundingBox();
    if (!box) throw new Error("settings body is not visible");
    await resetMetrics(page);
    const before = await chartSample(page);
    await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
    await body.evaluate((element) => element.scrollBy({ top: 600 }));
    await expect
      .poll(() => body.evaluate((element) => element.scrollTop))
      .toBeGreaterThan(0);
    const scrollbarClearance = await body.evaluate((element) => {
      const card = element.querySelector(".choice");
      if (!(card instanceof HTMLElement)) throw new Error("missing path card");
      return (
        element.getBoundingClientRect().right -
        card.getBoundingClientRect().right
      );
    });
    expect(scrollbarClearance).toBeGreaterThanOrEqual(12);
    await page.waitForTimeout(500);
    const after = await chartSample(page);
    const frameBudget = Math.ceil(((after.now - before.now) * 30) / 1000);
    expect(after.frames - before.frames).toBeLessThanOrEqual(frameBudget + 2);
    const metrics = await performanceMetrics(page);
    expect(metrics.frameP95).toBeLessThan(50);
    expect(metrics.longestTask).toBeLessThanOrEqual(100);
  },
);
performanceTest(
  "settings remain contained by their desktop dock",
  async ({ page }) => {
    await openApp(page, "dummy", { width: 1440, height: 900 });
    await openSettings(page);
    const body = page.locator('[aria-label="Settings"] .panel-body');
    await expectVisible(body);
    const desktopSurface = await body.evaluate((element) => ({
      overflowY: getComputedStyle(element).overflowY,
      panelBottom: element.closest(".panel")?.getBoundingClientRect().bottom,
      bodyBottom: element.getBoundingClientRect().bottom,
      statusTop: document.querySelector(".status")?.getBoundingClientRect().top,
    }));
    expect(desktopSurface.overflowY).toBe("auto");
    expect(desktopSurface.bodyBottom).toBeLessThanOrEqual(
      desktopSurface.panelBottom! + 1,
    );
    expect(desktopSurface.panelBottom).toBeLessThanOrEqual(
      desktopSurface.statusTop! + 1,
    );
  },
);
