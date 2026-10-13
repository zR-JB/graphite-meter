import { baseConfig, closeSettings, home, open, openSettings, phase, ready, run } from "./fleet";
import { expect, settled, test, type Locator, type Page } from "../../client/e2e/webview";

const tip = (page: Page) => page.locator('[role="tooltip"]');

async function centre(locator: Locator) {
  return locator.evaluate(async (el: Element) => {
    // A web font that arrives late reflows the top bar, moving the control from under a measured point.
    await document.fonts.ready;
    el.scrollIntoView({ block: "center" });
    await new Promise((done) => requestAnimationFrame(done));
    const box = el.getBoundingClientRect();
    return { x: box.x + box.width / 2, y: box.y + box.height / 2 };
  });
}

const moveMouse = (page: Page, x: number, y: number) => page.hover(x, y);

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

async function tap(page: Page, locator: Locator) {
  const { x, y } = await centre(locator);
  await page.touch("start", x, y);
  await page.touch("end");
}

test("a pause on a control opens its tip while the hand drifts, and leaving closes it", async (page) => {
  await open(page, home.http);
  const settings = page.getByRole("button", { name: "Settings", exact: true });
  const { x, y } = await centre(settings);
  // A reading hand never holds still: the tip must open while it drifts a few pixels every step. A busy runner's
  // timers fire late, so the hand drifts for a while rather than a number of steps.
  let opened = false;
  const until = performance.now() + 5_000;
  for (let step = 0; performance.now() < until && !opened; step++) {
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
  const icon = (name: string) => centre(page.getByRole("button", { name, exact: true }));
  const history = await icon("History");
  const details = await icon("Details");
  await page.evaluate(() => {
    const opened: string[] = ((window as any).opened = []);
    new MutationObserver((records) => {
      for (const record of records)
        for (const node of record.addedNodes)
          if ((node as Element).classList?.contains("tooltip")) opened.push(node.textContent ?? "");
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

test("explanations on click open only when asked, while a control still names itself on a rest", async (page) => {
  await open(page, home.http);
  const settings = await openSettings(page);
  await settings.getByRole("button", { name: "On click", exact: true }).click();
  await closeSettings(page);
  await page.reload();
  await expect.poll(() => page.evaluate(() => document.documentElement.dataset.explanations)).toBe("click");

  const fact = page.locator('.results .card[data-tone="latency"] dt[data-tip]');
  const { x, y } = await centre(fact);
  await moveMouse(page, x, y);
  await Bun.sleep(1_000);
  await expect(tip(page)).toHaveCount(0);
  await fact.click();
  await expect(tip(page)).toContainText("Stability");
  await fact.click();
  await expect(tip(page)).toHaveCount(0);

  const key = await centre(page.getByRole("button", { name: "Settings", exact: true }));
  await moveMouse(page, key.x, key.y);
  await expect(tip(page)).toContainText("Settings");
});

test.chrome("a stage chip explains itself over its cut label, on a rest and on a long press", async (page) => {
  await open(page, home.http);
  await run(page);
  const label = page.locator('.stage-track [data-tone="download"] .chip-label');
  await label.evaluate((node: HTMLElement) => Object.assign(node.style, { flex: "none", maxWidth: "2ch" }));
  await expect.poll(() => label.evaluate((node: HTMLElement) => node.dataset.tip)).toBe("");
  const { x, y } = await centre(label);
  await moveMouse(page, x, y);
  await expect(tip(page)).toContainText("Complete");
  await moveMouse(page, x, y + 240);
  await expect(tip(page)).toHaveCount(0);
  await page.cdp("Emulation.setTouchEmulationEnabled", { enabled: true, maxTouchPoints: 1 });
  await page.touch("start", x, y);
  await expect(tip(page)).toContainText("Complete");
  await page.touch("end");
});

test.chrome("a touch screen with a keyboard names the keys in the strip and in tips", async (page) => {
  await page.setViewportSize({ width: 1600, height: 900 });
  await page.cdp("Emulation.setTouchEmulationEnabled", { enabled: true, maxTouchPoints: 5 });
  await open(page, home.http);
  expect(await page.evaluate(() => matchMedia("(any-hover: none)").matches)).toBe(true);
  await expect(page.getByRole("group", { name: "Keyboard shortcuts" })).toBeVisible();
  const focused = () => page.evaluate(() => document.activeElement?.getAttribute("aria-label"));
  for (let stop = 0; stop < 8 && (await focused()) !== "Settings"; stop++) await page.press("Tab");
  expect(await focused()).toBe("Settings");
  await expect(tip(page)).toContainText("Settings (S)");
});

test.chrome("a finger's card terms are 24 px targets on a phone, and cards keep their layout", async (page) => {
  await open(page, home.http);
  await run(page);
  // Where each card's labels and figures sit, by their text.
  const layout = () =>
    page.locator(".results .card .facts").all((lists: HTMLElement[]) =>
      lists.flatMap((list) =>
        Array.from(list.querySelectorAll("dt, dd"), (node) => {
          const range = document.createRange();
          range.selectNodeContents(node);
          const { x, y, width, height } = range.getBoundingClientRect();
          return [x, y, width, height].map(Math.round);
        }),
      ),
    );
  // Wide cards stack their facts 22 px apart; a phone's wrap them in columns.
  for (const width of [1000, 390]) {
    await page.cdp("Emulation.setTouchEmulationEnabled", { enabled: false });
    await page.setViewportSize({ width, height: 900 });
    await page.evaluate(settled);
    const before = await layout();
    await page.cdp("Emulation.setTouchEmulationEnabled", { enabled: true, maxTouchPoints: 1 });
    expect(await page.evaluate(() => matchMedia("(pointer: coarse)").matches)).toBe(true);
    expect(await layout()).toEqual(before);
  }
  const targets = await page.locator(".results .card .facts dt[data-tip]").all((terms: HTMLElement[]) =>
    terms.map((term) => {
      term.scrollIntoView({ block: "center" });
      const box = term.getBoundingClientRect();
      const hits = (y: number) => term.contains(document.elementFromPoint(box.x + box.width / 2, y));
      const middle = box.y + box.height / 2;
      return { tall: box.height >= 24, reached: hits(middle - 11) && hits(middle + 11) };
    }),
  );
  expect(targets.length).toBeGreaterThan(0);
  for (const target of targets) expect(target).toEqual({ tall: true, reached: true });
});

test.chrome(
  "a finger scrolls past graphs, reads one by dragging sideways and toggles jargon by a tap",
  async (page) => {
    await page.setViewportSize({ width: 390, height: 844 });
    await page.cdp("Emulation.setTouchEmulationEnabled", {
      enabled: true,
      maxTouchPoints: 1,
    });
    await open(page, home.http);
    await run(page);
    const readout = page.locator(".graph .readout.shown");
    const stage = page.locator(".measurement-stage");
    const graph = page.locator(".results .graph");

    const before = await stage.evaluate((el: Element) => el.scrollTop);
    const from = await centre(graph);
    const scrolled = await stage.evaluate((el: Element) => {
      el.addEventListener("scrollend", () => Object.assign(window, { restedAt: el.scrollTop }));
      return el.scrollTop;
    });
    await page.touch("start", from.x, from.y);
    for (let step = 1; step <= 8; step++) await page.touch("move", from.x, from.y - step * 20);
    await page.touch("end");
    // The swipe flings on after the finger lifts; a drag begun mid-fling lands on moving content.
    await expect
      .poll(() => page.evaluate(() => (window as any).restedAt ?? -1))
      .toBeGreaterThan(Math.max(before, scrolled));
    await expect(readout).toHaveCount(0);

    const at = await centre(graph);
    await page.touch("start", at.x - 40, at.y);
    for (let step = 1; step <= 8; step++) await page.touch("move", at.x - 40 + step * 10, at.y);
    await expect(readout).toHaveCount(1);
    await page.touch("end");
    await expect(readout).toHaveCount(0);

    const stability = page.locator('.results .card[data-tone="latency"] dt[data-tip]');
    await tap(page, stability);
    await expect(tip(page)).toContainText("Stability");
    await tap(page, stability);
    await expect(tip(page)).toHaveCount(0);
  },
);

test.chrome("a phone's sheet leaves on a short quick flick and springs back from a slow short drag", async (page) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await page.cdp("Emulation.setTouchEmulationEnabled", {
    enabled: true,
    maxTouchPoints: 1,
  });
  await open(page, home.http);
  const sheet = page.locator("dialog.panel[open]");
  // Each move carries its own time, so the page reads the finger's speed whatever the delivery takes.
  async function drag(distance: number, ms: number) {
    const head = await page.locator("dialog.panel[open] .sheet-head").evaluate((el: Element) => {
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
  await expect.poll(() => sheet.evaluate((el: Element) => getComputedStyle(el).transform)).toBe("none");
  await expect(sheet).toHaveCount(1);

  await drag(60, 50);
  await expect(sheet).toHaveCount(0);
});

test.chrome(
  "a short phone's scrolled sheet scrolls back on a downward flick, and leaves from its head",
  async (page) => {
    await page.setViewportSize({ width: 390, height: 460 });
    await page.cdp("Emulation.setTouchEmulationEnabled", { enabled: true, maxTouchPoints: 1 });
    await open(page, home.http);
    const sheet = page.locator("dialog.panel[open]");
    async function flick(selector: string) {
      const from = await page.locator(selector).evaluate((el: Element) => {
        const box = el.getBoundingClientRect();
        return { x: box.x + box.width / 2, y: box.y + Math.min(box.height / 2, 40) };
      });
      let clock = Date.now();
      const at = (type: string, y?: number) =>
        page.cdp("Input.dispatchTouchEvent", {
          type,
          timestamp: clock / 1000,
          touchPoints: y === undefined ? [] : [{ x: from.x, y }],
        });
      await at("touchStart", from.y);
      for (let step = 1; step <= 6; step++) {
        clock += 8;
        await at("touchMove", from.y + step * 12);
      }
      clock += 8;
      await at("touchEnd");
    }
    await page.getByRole("button", { name: "Settings", exact: true }).click();
    await expect(sheet).toHaveCount(1);
    // Whichever box scrolls in this layout, scrolled from its top.
    await sheet.evaluate((dialog: HTMLElement) => {
      for (const box of [dialog, dialog.querySelector(".panel-body")!] as HTMLElement[]) box.scrollTop = 250;
    });
    await flick("dialog.panel[open] .panel-body");
    await Bun.sleep(300);
    await expect(sheet).toHaveCount(1);
    await flick("dialog.panel[open] .sheet-head");
    await expect(sheet).toHaveCount(0);
  },
);

test.chrome("a pinch on a phone's sheet zooms the page and leaves the sheet open", async (page) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await page.cdp("Emulation.setTouchEmulationEnabled", { enabled: true, maxTouchPoints: 2 });
  await open(page, home.http);
  const sheet = page.locator("dialog.panel[open]");
  await page.getByRole("button", { name: "Settings", exact: true }).click();
  await expect(sheet).toHaveCount(1);
  await page.evaluate(settled);
  let clock = Date.now();
  const touch = (type: string, points: { x: number; y: number; id: number }[]) =>
    page.cdp("Input.dispatchTouchEvent", { type, timestamp: (clock += 8) / 1000, touchPoints: points });
  // In the sheet's content: the lower finger first, then the upper, then they spread apart.
  await touch("touchStart", [{ x: 90, y: 600, id: 0 }]);
  await touch("touchStart", [
    { x: 90, y: 600, id: 0 },
    { x: 280, y: 280, id: 1 },
  ]);
  for (let step = 1; step <= 8; step++)
    await touch("touchMove", [
      { x: 90, y: 600 + step * 20, id: 0 },
      { x: 280, y: 280 - step * 20, id: 1 },
    ]);
  await touch("touchEnd", []);
  await Bun.sleep(700);
  await expect(sheet).toHaveCount(1);
  expect(await page.evaluate(() => visualViewport?.scale ?? 1)).toBeGreaterThan(1);
});

test("a graph's reading stays inside a narrow card wherever it reads", async (page) => {
  await page.setViewportSize({ width: 900, height: 800 });
  await open(page, home.http);
  await run(page);
  const graph = page.locator('.results .card[data-tone="download"] .graph');
  for (const at of [0.05, 0.5, 0.95]) {
    const box = await graph.evaluate((el: Element) => {
      el.scrollIntoView({ block: "center" });
      const r = el.getBoundingClientRect();
      return { x: r.x, y: r.y, width: r.width, height: r.height };
    });
    await page.hover(box.x + box.width * at, box.y + box.height * 0.4);
    await expect(page.locator(".results .graph .readout.shown")).toHaveCount(1);
    await expect
      .poll(() =>
        graph.evaluate((el: Element) => {
          const card = el.closest(".card")!.getBoundingClientRect();
          const plate = el.querySelector(".readout")!.getBoundingClientRect();
          return plate.left >= card.left && plate.right <= card.right;
        }),
      )
      .toBe(true);
  }
});

test.chrome("a phone's top bar keeps every key in view, with a live run seen from History", async (page) => {
  await page.cdp("Emulation.setTouchEmulationEnabled", { enabled: true, maxTouchPoints: 1 });
  await open(page, home.http, {
    config: { ...baseConfig, duration: { ...baseConfig.duration, latencyMs: 30_000 } },
  });
  await ready(page);
  const overflow = () =>
    page.evaluate(
      () =>
        Math.max(...[...document.querySelector(".topbar")!.children].map((key) => key.getBoundingClientRect().right)) -
        innerWidth,
    );
  const widths = [280, 300, 320, 340, 360, 390];
  for (const width of widths) {
    await page.setViewportSize({ width, height: 800 });
    expect(await overflow(), `idle at ${width}`).toBeLessThanOrEqual(0);
  }
  await page.press("r");
  await expect(phase(page, "latency")).toHaveCount(1, { timeout: 10_000 });
  await page.evaluate(() => (location.hash = "#/history"));
  await expect(page.locator(".return-live")).toHaveCount(1);
  for (const width of widths) {
    await page.setViewportSize({ width, height: 800 });
    expect(await overflow(), `live at ${width}`).toBeLessThanOrEqual(0);
  }
});

test("a sheet's head draws its rule once its content scrolls under it, in every engine", async (page) => {
  await page.setViewportSize({ width: 1280, height: 600 });
  await open(page, home.http);
  const settings = await openSettings(page);
  const rule = () =>
    settings.evaluate((sheet: HTMLElement) => {
      const head = sheet.querySelector(".sheet-head")!;
      return getComputedStyle(head).borderBottomColor;
    });
  const scroll = (top: number) =>
    settings.evaluate((sheet: HTMLElement, top: number) => {
      const body = sheet.querySelector<HTMLElement>(".sheet-body")!;
      body.scrollTop = top;
      body.dispatchEvent(new Event("scroll"));
    }, top);
  const clear = (color: string) => color === "transparent" || /, 0\)$/.test(color);
  expect(clear(await rule())).toBe(true);
  await scroll(60);
  await expect.poll(async () => clear(await rule())).toBe(false);
  await scroll(0);
  await expect.poll(async () => clear(await rule())).toBe(true);
});

test("the run's toast fades out as it fades in, in every engine", async (page) => {
  await open(page, home.http);
  await ready(page);
  const sample = await page.evaluate(async () => {
    const toast = document.querySelector<HTMLElement>(".phase-toast")!;
    const wait = (ms: number) => new Promise((done) => setTimeout(done, ms));
    toast.classList.add("visible");
    await wait(400);
    toast.classList.remove("visible");
    await wait(50);
    const leaving = getComputedStyle(toast);
    const during = [leaving.visibility, Number(leaving.opacity)] as const;
    await wait(400);
    return { during, after: getComputedStyle(toast).visibility };
  });
  expect(sample.during[0]).toBe("visible");
  expect(sample.during[1]).toBeGreaterThan(0);
  expect(sample.during[1]).toBeLessThan(1);
  expect(sample.after).toBe("hidden");
});
