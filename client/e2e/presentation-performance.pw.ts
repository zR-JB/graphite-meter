import { expect, test } from "@playwright/test";

type Metrics = {
  __canvasDraws: number;
  __chartFrames: number;
  __frameWork: number[];
  __longTasks: number[];
};

test.beforeEach(async ({ page }) => {
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
      // Firefox has no Long Tasks API.
    }
  });
});

const draws = (page: import("@playwright/test").Page) =>
  page.evaluate(() => (window as unknown as Metrics).__canvasDraws);

const chartSample = (page: import("@playwright/test").Page) =>
  page.evaluate(() => ({
    frames: (window as unknown as Metrics).__chartFrames,
    now: performance.now(),
  }));

const resetMetrics = (page: import("@playwright/test").Page) =>
  page.evaluate(() => {
    const metrics = window as unknown as Metrics;
    metrics.__frameWork.length = 0;
    metrics.__longTasks.length = 0;
  });

const performanceMetrics = (page: import("@playwright/test").Page) =>
  page.evaluate(() => {
    const metrics = window as unknown as Metrics;
    const sorted = [...metrics.__frameWork].sort((a, b) => a - b);
    return {
      frameP95: sorted[Math.floor(sorted.length * 0.95)] ?? 0,
      longestTask: Math.max(0, ...metrics.__longTasks),
    };
  });

test("canvas work parks when settled or offscreen", async ({
  page,
  browserName,
  context,
}) => {
  if (browserName === "chromium") {
    const session = await context.newCDPSession(page);
    await session.send("Emulation.setCPUThrottlingRate", { rate: 4 });
  }
  await page.goto("/?engine=dummy");
  await page.waitForTimeout(1800);

  const idle = await draws(page);
  await page.waitForTimeout(500);
  expect(await draws(page)).toBe(idle);

  await resetMetrics(page);
  await page.getByRole("button", { name: "Start the speed test" }).click();
  await page.waitForTimeout(1200);
  const plot = page.locator(
    '[role="img"][aria-label="Throughput and latency over time"]',
  );
  await plot.scrollIntoViewIfNeeded();
  const box = await plot.boundingBox();
  if (!box) throw new Error("chart is not visible");

  const active = await chartSample(page);
  for (let i = 0; i < 40; i++)
    await page.mouse.move(box.x + (box.width * i) / 40, box.y + box.height / 2);
  await page.waitForTimeout(1000);
  const afterPointer = await chartSample(page);
  const frameBudget = Math.ceil(((afterPointer.now - active.now) * 30) / 1000);
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

  await page.getByRole("button", { name: "Abort test" }).click();
  await page.waitForTimeout(1800);
  const aborted = await draws(page);
  await page.waitForTimeout(500);
  expect(await draws(page)).toBe(aborted);

  const metrics = await performanceMetrics(page);
  expect(metrics.frameP95).toBeLessThan(50);
  // Chromium only reports long tasks once they cross 50 ms, making a 50 ms
  // ceiling equivalent to requiring none on a shared runner. Under the 4x CPU
  // throttle, 100 ms still rejects presentation stalls without runner jitter.
  expect(metrics.longestTask).toBeLessThanOrEqual(100);
});

test("completed reduced-motion views settle after theme and resize", async ({
  page,
}) => {
  await page.emulateMedia({ reducedMotion: "reduce" });
  await page.goto("/?engine=dummy");
  await page.getByRole("button", { name: "Open settings" }).click();
  const settings = page.locator('[aria-label="Settings"]');
  await settings.getByRole("button", { name: "custom" }).click();
  for (const [label, value] of [
    ["Warmup ms", "0"],
    ["Latency ms", "300"],
    ["Download ms", "300"],
    ["Upload ms", "300"],
  ] as const)
    await settings.getByLabel(label).fill(value);

  await page.getByRole("button", { name: "Start the speed test" }).click();
  await expect(
    page.getByRole("button", { name: "Run the test again" }),
  ).toBeVisible({ timeout: 5000 });
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
});
