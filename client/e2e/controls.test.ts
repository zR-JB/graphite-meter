import {
  baseConfig,
  catalog,
  downloadOnly,
  frankfurt,
  open,
  openSettings,
  phase,
  ready,
  run,
  runButton,
  savedResult,
  spawnPeer,
} from "./fleet";
import { browser, expect, test, type Locator } from "./webview";

const checked = (locator: Locator) => locator.evaluate((el: HTMLInputElement) => el.checked);

test("reset settings confirms, preserves on cancel and restores defaults", async (page) => {
  await open(page);
  const settings = await openSettings(page);
  await settings.getByRole("button", { name: "Custom", exact: true }).click();
  const warmup = settings.getByRole("spinbutton", { name: "Warmup time" });
  await warmup.fill("1.2");
  await settings.getByRole("button", { name: "Bytes", exact: true }).click();
  const wireLabel = "Show estimated wire rate";
  const wire = settings.getByRole("switch", { name: wireLabel });
  await settings.locator("label.switch", { hasText: wireLabel }).click();
  expect(await checked(wire)).toBe(false);

  const reset = settings.getByRole("button", { name: "Reset settings" });
  await reset.click();
  const dialog = page.getByRole("alertdialog", { name: "Reset settings?" });
  const keep = dialog.getByRole("button", { name: "Keep settings" });
  await expect(keep).toBeFocused();
  await keep.click();
  await expect(dialog).toHaveCount(0);
  await expect(reset).toBeFocused();
  await expect(warmup).toHaveValue("1.2 s");

  await reset.click();
  await dialog.getByRole("button", { name: "Reset settings" }).click();
  await expect(dialog).toHaveCount(0);
  const pressed = (name: string) =>
    expect(settings.getByRole("button", { name, exact: true })).toHaveAttribute(
      "aria-pressed",
      "true",
    );
  await pressed("Medium");
  await pressed("Bits");
  await pressed("Decimal");
  expect(await checked(wire)).toBe(true);
});

test("an arrow key or a step key steps the time typed into a field, not the one it held", async (page) => {
  await open(page);
  const settings = await openSettings(page);
  await settings.getByRole("button", { name: "Custom", exact: true }).click();
  const download = settings.getByRole("spinbutton", { name: "Download stage time" });
  // Typed, not yet committed: no change event until the field is left.
  const type = (text: string) =>
    download.evaluate((field: HTMLInputElement, text: string) => {
      field.focus();
      field.value = text;
      field.dispatchEvent(new Event("input", { bubbles: true }));
    }, text);
  await download.fill("4");
  await expect(download).toHaveValue("4 s");
  // Over 4 s, a typed 2 steps up to 2.5 s, not to the 4.5 s after the 4 it held.
  await type("2");
  await page.press("ArrowUp");
  await expect(download).toHaveValue("2.5 s");
  // Text that reads as no time steps the time as it was.
  await type("soon");
  await page.press("ArrowUp");
  await expect(download).toHaveValue("3 s");
  // The key beside the field steps the typed time too: 20 typed over 3 s lengthens from 20 s.
  await type("20");
  await settings.getByRole("button", { name: "Download stage time, increase" }).click();
  await expect(download).toHaveValue("21 s");
});

test("a unit switch relabels the dial in place, on round ticks, without replaying the result", async (page) => {
  await open(page, undefined, { config: downloadOnly });
  await ready(page);
  // The run key is never marked busy, as a start it prepares can still be canceled.
  await page.evaluate(() => {
    const key = document.querySelector(".run-button")!;
    const busy: (string | null)[] = [];
    new MutationObserver((records) =>
      busy.push(...records.map(() => key.getAttribute("aria-busy"))),
    ).observe(key, {
      attributeFilter: ["aria-busy"],
    });
    Object.assign(window, { busy });
  });
  await run(page);
  expect(await page.evaluate(() => (window as any).busy)).toEqual([]);
  expect(
    await runButton(page, "Run again").evaluate((key: Element) => key.hasAttribute("aria-busy")),
  ).toBe(false);
  const sweeps = () =>
    page.evaluate(
      () =>
        document
          .getAnimations()
          .filter(
            (animation) =>
              (animation as CSSAnimation).animationName === "result-sweep" &&
              animation.playState === "running",
          ).length,
    );
  // The result takes the dial a hand-off after the run completes; its sweep ends before the switch.
  await expect
    .poll(() => page.locator(".result-arc").all((paths: Element[]) => paths.length))
    .toBeGreaterThan(0);
  await expect.poll(sweeps).toBe(0);
  const settings = await openSettings(page);
  await settings.getByRole("button", { name: "Bytes", exact: true }).click();
  await expect(page.locator(".metric-wrap")).toContainText("B/s");
  expect(await sweeps()).toBe(0);
  const labels = await page
    .locator(".gauge-ticks")
    .evaluate((el: HTMLElement) =>
      Array.from(el.querySelectorAll(".gauge-tick"), (tick) => tick.textContent),
    );
  expect(labels.length).toBeGreaterThan(1);
  for (const label of labels) expect(label).toMatch(/^\d+(\.\d{1,2})?$/);
});

test("Escape closes a settings confirmation; Back closes it with its panel", async (page) => {
  await open(page);
  const settings = await openSettings(page);
  const reset = settings.getByRole("button", { name: "Reset settings" });
  const dialog = page.getByRole("alertdialog", { name: "Reset settings?" });
  await reset.click();
  await expect(dialog).toBeVisible();
  await page.press("Escape");
  await expect(dialog).toHaveCount(0);
  await expect(reset).toBeFocused();

  await reset.click();
  await expect(dialog).toBeVisible();
  await page.evaluate(() => history.back());
  await expect(dialog).toHaveCount(0);
  await expect
    .poll(() => settings.all((els: HTMLElement[]) => els.every((el) => el.inert)))
    .toBe(true);
  await expect(page.getByRole("button", { name: "Settings", exact: true })).toBeFocused();
});

test("Escape closes the docked panel holding focus and never stops a running test", async (page) => {
  await page.setViewportSize({ width: 1600, height: 900 });
  await open(page, undefined, {
    config: { duration: { ...baseConfig.duration, downloadMs: 20_000 } },
  });
  await ready(page);
  await runButton(page, "Start test").click();
  // Escape cancels a start still proving its paths, so the panels open once the run is under way.
  await expect(phase(page, "idle")).toHaveCount(0);
  const settings = await openSettings(page);
  const details = page.locator('dialog[aria-label="Details"]');
  await page.getByRole("button", { name: "Details", exact: true }).click();
  const closed = (panel: typeof settings) =>
    expect.poll(() => panel.all((els: HTMLElement[]) => els.every((el) => el.inert))).toBe(true);
  await settings
    .getByRole("button", { name: "Close Settings" })
    .evaluate((el: HTMLElement) => el.focus());
  await page.press("Escape");
  await closed(settings);
  expect(await details.all((els: HTMLElement[]) => els.some((el) => !el.inert))).toBe(true);
  // Focus returns to the Settings button, whose tip takes the first Escape.
  await page.press("Escape");
  await page.press("Escape");
  await closed(details);
  await page.press("Escape");
  await expect(runButton(page, "Stop test")).toBeVisible();
  await runButton(page, "Stop test").click();
  await expect(phase(page, "aborted")).toHaveCount(1);
});

test("a docked panel's edge steps 16 px from the keyboard, within its limits", async (page) => {
  await page.setViewportSize({ width: 1600, height: 900 });
  await open(page);
  const settings = await openSettings(page);
  const handle = settings.getByRole("separator", {
    name: /^Resize Settings panel/,
  });
  await handle.evaluate((el: HTMLElement) => el.focus());
  for (const [key, width] of [
    ["ArrowRight", "436"],
    ["Home", "360"],
    ["End", "720"],
    ["Enter", "420"],
  ] as const) {
    await page.press(key);
    await expect(handle).toHaveAttribute("aria-valuenow", width);
  }
});

test.chrome("a held shortcut acts on its press, and its repeats do nothing", async (page) => {
  await open(page);
  await expect(page.locator("#console")).toHaveCount(1);
  const key = (type: string, autoRepeat = false) =>
    page.cdp("Input.dispatchKeyEvent", {
      type,
      key: "h",
      code: "KeyH",
      text: type === "keyUp" ? undefined : "h",
      windowsVirtualKeyCode: 72,
      autoRepeat,
    });
  await key("keyDown");
  // Five repeats: had each toggled, History would end closed.
  for (let i = 0; i < 5; i++) await key("keyDown", true);
  await key("keyUp");
  await expect(page.locator(".history-workspace")).toHaveCount(1);
  expect(await page.evaluate(() => location.hash)).toContain("history");
});

test("Space and R run the test from page load; a focused control keeps Space", async (page) => {
  await open(page, undefined, {
    config: { duration: { ...baseConfig.duration, downloadMs: 20_000 } },
  });
  await ready(page);
  // As after a fresh load: focus left the Settings button that ready() used.
  await page.evaluate(() => (document.activeElement as HTMLElement).blur());
  await page.press(" ");
  // Stop shows from the press; the run is under way once its first stage has started.
  await expect(runButton(page, "Stop test")).toBeVisible();
  await expect(
    page.locator('#console:is([data-phase="warmup"], [data-phase="latency"])'),
  ).toHaveCount(1);
  await page.press("r");
  await expect(phase(page, "aborted")).toHaveCount(1);
  await page
    .getByRole("switch", { name: /^Upload stage/ })
    .evaluate((el: HTMLElement) => el.focus());
  await page.press(" ");
  await expect(runButton(page, "Run again")).toBeVisible();
  await expect(phase(page, "aborted")).toHaveCount(1);
});

test("legal notices recover through Retry and keep focus in the dialog", async (page) => {
  await open(page);
  await page.blockRequests(["*legal/about.json*"]);
  await page.getByRole("button", { name: "Details" }).click();
  await page.getByRole("button", { name: "About & legal" }).click();
  const dialog = page.getByRole("dialog", { name: "About & legal" });
  await expect(dialog).toContainText("Could not load legal notices.");
  await page.blockRequests([]);
  await dialog.getByRole("button", { name: "Retry" }).click();
  await expect(dialog).toContainText("Third-party software");

  // A link's label never breaks, at any width.
  for (const width of [1280, 600, 390]) {
    await page.setViewportSize({ width, height: 900 });
    const wrapped = await dialog.locator(".where a").all((links) =>
      links
        .filter((link) => {
          const label = document.createRange();
          label.selectNodeContents(link.querySelector("span")!);
          return label.getClientRects().length !== 1;
        })
        .map((link) => link.textContent),
    );
    expect(wrapped).toEqual([]);
  }

  const link = dialog.locator(".group:last-child .component:last-child .where a:last-child");
  const close = dialog.getByRole("button", { name: "Close About & legal" });
  await link.evaluate((el: HTMLElement) => el.focus());
  // Past the last control a modal hands focus to the browser, never the page; only Chrome has its UI headless.
  if (browser !== "chrome") return;
  await page.press("Tab");
  expect(await page.evaluate(() => document.activeElement?.tagName)).toBe("BODY");
  await page.press("Tab");
  await expect(close).toBeFocused();
});

test("the topbar menu opens, moves, acts and closes from the keyboard", async (page) => {
  await page.setViewportSize({ width: 300, height: 700 });
  await open(page);
  const trigger = page.getByRole("button", { name: "More controls" });
  const menu = page.getByRole("menu", { name: "More controls" });
  const items = menu.locator("[data-more]");
  await trigger.evaluate((el: HTMLElement) => el.focus());
  await page.press("ArrowDown");
  await expect(items.nth(0)).toBeFocused();
  await page.press("ArrowDown");
  await expect(items.nth(1)).toBeFocused();
  await page.press("Escape");
  await expect(menu).toHaveCount(0);
  await expect(trigger).toBeFocused();

  await page.press("ArrowUp");
  await expect(items.nth(1)).toContainText("Details");
  await expect(items.nth(1)).toBeFocused();
  await page.press("Enter");
  await expect(menu).toHaveCount(0);
  await expect
    .poll(() =>
      page
        .locator('.panel[aria-label="Details"]')
        .all((els: HTMLElement[]) => els.some((el) => !el.inert)),
    )
    .toBe(true);
});

test("the first result saves after the application server becomes unreachable", async (page) => {
  const oslo = await spawnPeer("Oslo", catalog(frankfurt));
  try {
    await open(page, oslo.server.url, {
      servers: [frankfurt],
      config: {
        stages: { ...baseConfig.stages, latency: false, upload: false },
        skipLoadedLatencyWhenStageOff: true,
        duration: { ...baseConfig.duration, downloadMs: 1500 },
      },
    });
    await ready(page);
    const startedAt = Date.now();
    await runButton(page, "Start test").click();
    await expect(phase(page, "download")).toHaveCount(1, { timeout: 10_000 });
    oslo.kill("SIGKILL");
    const saved = await savedResult(page, startedAt, 20_000);
    expect(saved.result.stages.download).toBe("complete");
  } finally {
    oslo.kill();
  }
});

test("a docked sheet reopened while it leaves comes back from where it was", async (page) => {
  await page.setViewportSize({ width: 1600, height: 900 });
  await open(page);
  const travel = await page.evaluate(async () => {
    const key = (key: string) =>
      document.dispatchEvent(new KeyboardEvent("keydown", { key, bubbles: true }));
    const frame = () => new Promise((done) => requestAnimationFrame(done));
    const left = () =>
      document.querySelector('dialog[aria-label="Details"]')!.getBoundingClientRect().left;
    key("d");
    let docked = NaN;
    for (let n = 0, was = NaN; n < 300 && !(docked < innerWidth && docked === was); n++) {
      await frame();
      [was, docked] = [docked, left()];
    }
    key("d");
    // Mid-way out it is still on screen.
    let leaving = false;
    for (let n = 0; n < 60 && !leaving; n++) {
      await frame();
      leaving = left() > docked && left() < innerWidth;
    }
    key("d");
    const back: number[] = [];
    const end = performance.now() + 500;
    while (performance.now() < end) {
      await frame();
      back.push(left());
    }
    return { leaving, back, width: innerWidth };
  });
  expect(travel.leaving).toBe(true);
  expect(Math.min(...travel.back)).toBeGreaterThan(travel.width / 2);
});

test("two docked sheets open and close beside the stage in every frame", async (page) => {
  await page.setViewportSize({ width: 1700, height: 900 });
  await open(page);
  const overlaps = await page.evaluate(async () => {
    const key = (key: string) =>
      document.dispatchEvent(new KeyboardEvent("keydown", { key, bubbles: true }));
    const wait = (ms: number) => new Promise((done) => setTimeout(done, ms));
    const found: string[] = [];
    let sampling = true;
    const sheets = (side: string) =>
      [...document.querySelectorAll(`dialog.panel.${side}`)]
        .filter((el) => el.checkVisibility())
        .map((el) => el.getBoundingClientRect())
        .filter((box) => box.right > 0 && box.left < innerWidth);
    const sampled = (async () => {
      while (sampling) {
        await new Promise((done) => requestAnimationFrame(done));
        const stage = document.querySelector('[data-flip="stage"]')!.getBoundingClientRect();
        const run = document.querySelector(".run-button")!.getBoundingClientRect();
        for (const box of sheets("left"))
          if (box.right > stage.left + 1.5)
            found.push(`left ${box.left} to ${box.right}, stage ${stage.left}`);
        for (const box of sheets("right"))
          if (box.left < stage.right - 1.5)
            found.push(`right ${box.left} to ${box.right}, stage ${stage.right}`);
        if (run.left < stage.left - 1.5 || run.right > stage.right + 1.5)
          found.push(`run key ${run.left} to ${run.right} outside the stage`);
      }
    })();
    for (const [name, ms] of [
      ["s", 600],
      ["d", 600],
      ["s", 600],
      ["d", 600],
      ["s", 120],
      ["d", 120],
      ["s", 120],
      ["d", 600],
    ] as const) {
      key(name);
      await wait(ms);
    }
    sampling = false;
    await sampled;
    return found.slice(0, 5);
  });
  expect(overlaps).toEqual([]);
});

test("Details hides this browser's address until its key is pressed, whatever the pointer rests on", async (page) => {
  await open(page);
  await ready(page);
  await page.getByRole("button", { name: "Details", exact: true }).click();
  // A busy runner can still be sliding the sheet in, so wait for timed animations; scroll-driven ones never finish.
  await page.evaluate(() =>
    Promise.all(
      document
        .getAnimations()
        .filter((a) => Number.isFinite(a.effect?.getComputedTiming().endTime))
        .map((a) => a.finished),
    ),
  );
  const key = page.getByRole("button", { name: "Show your address" });
  await expect(key).toHaveAttribute("aria-pressed", "false");
  const address = page.locator(".secret-value");
  await expect(address).toHaveCount(0);
  await page.locator(".secret-mask").hover();
  await key.hover();
  // The key's own tip opens on a rest, so the pointer has rested on it.
  await expect(page.getByRole("tooltip")).toContainText("Show your address");
  await expect(address).toHaveCount(0);
  await key.click();
  await expect(key).toHaveAttribute("aria-pressed", "true");
  await expect(address).toContainText(/\d/);
  await key.click();
  await expect(key).toHaveAttribute("aria-pressed", "false");
  await expect(address).toHaveCount(0);
});

test.chrome(
  "a swipe that starts on a stepper key scrolls Settings without stepping; a tap steps once",
  async (page) => {
    await page.setViewportSize({ width: 390, height: 844 });
    await open(page);
    const settings = await openSettings(page);
    const field = settings.getByRole("spinbutton", { name: "Streams per direction" });
    const value = async () => (await field.state())[0]!.value;
    const before = await value();
    const box = await settings
      .getByRole("button", { name: "Streams per direction, increase" })
      .evaluate((key: HTMLElement) => {
        key.scrollIntoView({ block: "center" });
        return key.getBoundingClientRect().toJSON();
      });
    const [x, y] = [box.x + box.width / 2, box.y + box.height / 2];
    await page.touch("start", x, y);
    // Upwards, so the sheet's content scrolls rather than the sheet being dragged away.
    for (let step = 1; step <= 8; step++) await page.touch("move", x, y - step * 15);
    await page.touch("end", x, y - 120);
    // A tap during the swipe's fling only stops the fling, on a phone too, so the next one waits for it to rest.
    let top = Number.NaN;
    const resting = async () => {
      const at = await settings
        .getByRole("button", { name: "Streams per direction, increase" })
        .evaluate((key: HTMLElement) => key.getBoundingClientRect().top);
      return top === (top = at);
    };
    await expect.poll(resting).toBe(true);
    expect(await value()).toBe(before);
    const key = await settings
      .getByRole("button", { name: "Streams per direction, increase" })
      .evaluate((key: HTMLElement) => {
        const rect = key.getBoundingClientRect();
        return [rect.x + rect.width / 2, rect.y + rect.height / 2];
      });
    await page.touch("start", key[0]!, key[1]!);
    await page.touch("end", key[0]!, key[1]!);
    await expect.poll(value).toBe(String(Number(before) + 1));
  },
);
