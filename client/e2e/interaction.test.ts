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

type Point = { x: number; y: number };

// A hand crossing at a normal speed: 12 px a step, a step every 16 ms or so.
async function sweep(page: Page, from: Point, to: Point) {
  const steps = Math.ceil(Math.hypot(to.x - from.x, to.y - from.y) / 12);
  for (let step = 1; step <= steps; step++) {
    const at = (a: number, b: number) => a + ((b - a) * step) / steps;
    await moveMouse(page, at(from.x, to.x), at(from.y, to.y));
    await Bun.sleep(16);
  }
}

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

test("a hand sweeping across tips opens none, even just after one showed, and a rest opens one", async (page) => {
  await open(page, home.http);
  const icon = (name: string) =>
    centre(page.getByRole("button", { name, exact: true }));
  const history = await icon("History");
  const details = await icon("Details");
  await page.evaluate(() => {
    const opened: string[] = ((window as any).opened = []);
    new MutationObserver((records) => {
      for (const record of records)
        for (const node of record.addedNodes)
          if ((node as Element).classList?.contains("tooltip"))
            opened.push(node.textContent ?? "");
    }).observe(document.body, { childList: true });
  });
  const opened = () => page.evaluate<string[]>(() => (window as any).opened);
  // History, Theme and Details sit side by side; the sweep starts and ends off them.
  const left = { x: history.x - 80, y: history.y };
  const right = { x: details.x + 20, y: details.y };
  await sweep(page, left, right);
  await sweep(page, right, left);
  expect(await opened()).toEqual([]);
  await moveMouse(page, history.x, history.y);
  await expect(tip(page)).toContainText("History");
  // Its neighbours need a rest of their own, so sweeping on across them opens neither.
  await sweep(page, history, right);
  await expect(tip(page)).toHaveCount(0);
  expect((await opened()).length).toBe(1);
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
  const scrolled = await stage.evaluate((el: Element) => {
    el.addEventListener("scrollend", () =>
      Object.assign(window, { restedAt: el.scrollTop }),
    );
    return el.scrollTop;
  });
  await touch(page, "touchStart", from.x, from.y);
  for (let step = 1; step <= 8; step++)
    await touch(page, "touchMove", from.x, from.y - step * 20);
  await touch(page, "touchEnd");
  // The swipe flings on after the finger lifts; a drag begun mid-fling lands on moving content.
  await expect
    .poll(() => page.evaluate(() => (window as any).restedAt ?? -1))
    .toBeGreaterThan(Math.max(before, scrolled));
  await expect(readout).toHaveCount(0);

  const at = await centre(graph);
  await touch(page, "touchStart", at.x - 40, at.y);
  for (let step = 1; step <= 8; step++)
    await touch(page, "touchMove", at.x - 40 + step * 10, at.y);
  await expect(readout).toHaveCount(1);
  await touch(page, "touchEnd");
  await expect(readout).toHaveCount(0);

  const stability = page.locator(
    '.results .card[data-tone="latency"] dt[data-tip]',
  );
  await tap(page, stability);
  await expect(tip(page)).toContainText("Stability");
  await tap(page, stability);
  await expect(tip(page)).toHaveCount(0);
});

test("a phone's sheet leaves on a short quick flick and springs back from a slow short drag", async (page) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await page.cdp("Emulation.setTouchEmulationEnabled", {
    enabled: true,
    maxTouchPoints: 1,
  });
  await open(page, home.http);
  const sheet = page.locator("dialog.panel[open]");
  // Each move carries its own time, so the page reads the finger's speed whatever the delivery takes.
  async function drag(distance: number, ms: number) {
    const head = await page
      .locator("dialog.panel[open] .sheet-head")
      .evaluate((el: Element) => {
        const box = el.getBoundingClientRect();
        return { x: box.x + box.width / 2, y: box.y + box.height / 2 };
      });
    let clock = Date.now();
    const at = (type: string, y?: number) =>
      page.cdp("Input.dispatchTouchEvent", {
        type,
        timestamp: clock / 1000,
        touchPoints: y === undefined ? [] : [{ x: head.x, y }],
      });
    await at("touchStart", head.y);
    const steps = Math.round(ms / 8);
    for (let step = 1; step <= steps; step++) {
      clock += ms / steps;
      await at("touchMove", head.y + (distance * step) / steps);
    }
    clock += 8;
    await at("touchEnd");
  }

  await page.getByRole("button", { name: "Settings", exact: true }).click();
  await expect(sheet).toHaveCount(1);
  await drag(120, 1200);
  await expect
    .poll(() => sheet.evaluate((el: Element) => getComputedStyle(el).transform))
    .toBe("none");
  await expect(sheet).toHaveCount(1);

  await drag(60, 50);
  await expect(sheet).toHaveCount(0);
});
