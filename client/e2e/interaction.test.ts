import { home, open, run } from "./fleet";
import { expect, test, type Locator, type Page } from "./webview";

const tip = (page: Page) => page.locator('[role="tooltip"]');

async function centre(locator: Locator) {
  return locator.evaluate(async (el: Element) => {
    el.scrollIntoView({ block: "center" });
    await new Promise((done) => requestAnimationFrame(done));
    const box = el.getBoundingClientRect();
    return { x: box.x + box.width / 2, y: box.y + box.height / 2 };
  });
}

const moveMouse = (page: Page, x: number, y: number) =>
  page.cdp("Input.dispatchMouseEvent", { type: "mouseMoved", x, y });

const touch = (page: Page, type: string, x = 0, y = 0) =>
  page.cdp("Input.dispatchTouchEvent", {
    type,
    touchPoints: type === "touchEnd" ? [] : [{ x, y }],
  });

async function tap(page: Page, locator: Locator) {
  const { x, y } = await centre(locator);
  await touch(page, "touchStart", x, y);
  await touch(page, "touchEnd");
}

test("a pause on a control opens its tip while the hand drifts, and leaving closes it", async (page) => {
  await open(page, home.http);
  const settings = page.getByRole("button", { name: "Settings", exact: true });
  const { x, y } = await centre(settings);
  // A reading hand never holds still: the tip must open while it drifts a few pixels every step.
  let opened = false;
  for (let step = 0; step < 20 && !opened; step++) {
    await moveMouse(page, x - 3 + (step % 3) * 3, y - 2 + (step % 2) * 4);
    await Bun.sleep(60);
    opened = (await tip(page).state()).length > 0;
  }
  expect(opened).toBe(true);
  await expect(tip(page)).toContainText("Settings");
  await moveMouse(page, x, y + 240);
  await expect(tip(page)).toHaveCount(0);
});

test("a finger scrolls past graphs, reads one by dragging sideways and toggles jargon by a tap", async (page) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await page.cdp("Emulation.setTouchEmulationEnabled", {
    enabled: true,
    maxTouchPoints: 1,
  });
  await open(page, home.http);
  await run(page);
  const readout = page.locator(".graph .readout");
  const stage = page.locator(".measurement-stage");
  const graph = page.locator(".results .graph");

  const before = await stage.evaluate((el: Element) => el.scrollTop);
  const from = await centre(graph);
  const scrolled = await stage.evaluate((el: Element) => el.scrollTop);
  await touch(page, "touchStart", from.x, from.y);
  for (let step = 1; step <= 8; step++)
    await touch(page, "touchMove", from.x, from.y - step * 20);
  await touch(page, "touchEnd");
  await expect
    .poll(() => stage.evaluate((el: Element) => el.scrollTop))
    .toBeGreaterThan(Math.max(before, scrolled));
  await expect(readout).toHaveCount(0);

  const at = await centre(graph);
  await touch(page, "touchStart", at.x - 80, at.y);
  for (let step = 1; step <= 8; step++)
    await touch(page, "touchMove", at.x - 80 + step * 20, at.y);
  await expect(readout).toHaveCount(1);
  await touch(page, "touchEnd");
  await expect(readout).toHaveCount(0);

  const jitter = page.locator('.latency-card dt[data-tip="term"]');
  await tap(page, jitter);
  await expect(tip(page)).toContainText("Jitter");
  await tap(page, jitter);
  await expect(tip(page)).toHaveCount(0);
});
