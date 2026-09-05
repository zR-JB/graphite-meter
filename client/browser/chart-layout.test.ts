import {
  expect,
  openApp,
  prepareApp,
  startAndWait,
  startTest,
  test,
} from "./webview";
test("chart axes and time ticks are DOM labels anchored inside the canvas layout", async ({
  page,
}) => {
  await openApp(page);
  await startTest(page);
  const plot = page.locator(
    '[role="slider"][aria-label="Throughput and latency over time"]',
  );
  await expect(plot.locator(".chart-labels .time-label").first()).toBeVisible({
    timeout: 5000,
  });
  await expect(plot.locator(".chart-labels .axis-label").first()).toBeVisible();
  const geometry = await plot.evaluate((element) => {
    const canvas = element.querySelector("canvas");
    const tick = element.querySelector(".time-label");
    const axis = element.querySelector(".axis-label");
    if (!(canvas && tick && axis)) return null;
    const canvasBox = canvas.getBoundingClientRect();
    const tickBox = tick.getBoundingClientRect();
    const axisBox = axis.getBoundingClientRect();
    return {
      tickWithinCanvas:
        tickBox.left >= canvasBox.left && tickBox.right <= canvasBox.right,
      axisWithinCanvas:
        axisBox.left >= canvasBox.left && axisBox.right <= canvasBox.right,
    };
  });
  expect(geometry).toEqual({ tickWithinCanvas: true, axisWithinCanvas: true });
});
test("pinch zoom raises canvas resolution without changing layout", async ({
  page,
  context,
}) => {
  await openApp(page);
  const ratios = () =>
    page.locator("canvas").evaluateAll((canvases) =>
      canvases.map((canvas) => ({
        ratio: canvas.width / canvas.getBoundingClientRect().width,
        width: canvas.getBoundingClientRect().width,
      })),
    );
  const before = await ratios();
  const session = await context.newCDPSession(page);
  try {
    await session.send("Emulation.setPageScaleFactor", { pageScaleFactor: 2 });
    await expect
      .poll(async () => (await ratios()).every((canvas) => canvas.ratio > 1))
      .toBe(true);
    const after = await ratios();
    expect(after.map((canvas) => canvas.width)).toEqual(
      before.map((canvas) => canvas.width),
    );
    expect(after.every((canvas) => canvas.ratio <= 4)).toBe(true);
  } finally {
    await session.send("Emulation.setPageScaleFactor", { pageScaleFactor: 1 });
  }
});

test("chart inspector exposes the same bucket details to keyboard and touch", async ({
  page,
}) => {
  await page.emulateMedia({ reducedMotion: "reduce" });
  const settings = await prepareApp(page, "long-latency", "dummy", {
    width: 360,
    height: 740,
  });
  await settings.getByRole("button", { name: "Close Settings" }).click();
  const plot = page.getByRole("slider", {
    name: "Throughput and latency over time",
  });
  await expect(plot).toHaveAttribute("aria-disabled", "true");
  await startAndWait(page);
  await plot.focus();
  await plot.press("Home");
  await expect(plot).toHaveAttribute("aria-valuenow", "0");
  await plot.press("ArrowRight");
  expect(Number(await plot.getAttribute("aria-valuenow"))).toBeGreaterThan(0);
  await plot.press("End");
  expect(await plot.getAttribute("aria-valuenow")).toBe(
    await plot.getAttribute("aria-valuemax"),
  );
  // Inspect the same measured latency bucket with keyboard and touch.
  for (let i = 0; i < 40; i++) await plot.press("ArrowLeft");
  await expect(plot.locator(".chip")).toBeVisible();
  await expect(plot).toHaveAttribute(
    "aria-valuetext",
    /bucket median latency.*probe timeouts/,
  );
  await expect(plot.locator(".chip")).toContainText("probe timeouts");
  // Hover work is coalesced by the presentation scheduler; capture the final
  // key selection after its repaint rather than the previous key's chip.
  await page.evaluate(
    () =>
      new Promise<void>((resolve) =>
        requestAnimationFrame(() => requestAnimationFrame(() => resolve())),
      ),
  );
  const keyboardText = await plot.getAttribute("aria-valuetext");
  const fraction = await plot.evaluate(
    (el) =>
      Number(el.getAttribute("aria-valuenow")) /
      Number(el.getAttribute("aria-valuemax")),
  );
  await plot.press("Escape");
  await expect(plot.locator(".chip")).toHaveCount(0);
  const box = await plot.boundingBox();
  await plot.dispatchEvent("pointerup", {
    pointerType: "touch",
    clientX: box!.x + 46 + fraction * (box!.width - 92),
    clientY: box!.y + 50,
  });
  await expect(plot.locator(".chip")).toBeVisible();
  await expect(plot).toHaveAttribute("aria-valuetext", keyboardText!);
  await plot.dispatchEvent("pointerleave", { pointerType: "touch" });
  await expect(plot.locator(".chip")).toBeVisible();
  const untouched = await plot.evaluate((element) => {
    const key = new KeyboardEvent("keydown", {
      key: "a",
      bubbles: true,
      cancelable: true,
    });
    const scroll = new PointerEvent("pointermove", {
      pointerType: "touch",
      bubbles: true,
      cancelable: true,
    });
    element.dispatchEvent(key);
    element.dispatchEvent(scroll);
    return {
      key: key.defaultPrevented,
      scroll: scroll.defaultPrevented,
      touchAction: getComputedStyle(element).touchAction,
    };
  });
  expect(untouched).toEqual({ key: false, scroll: false, touchAction: "auto" });
  await page.artifact("chart-touch-inspector-narrow");
  const chip = await plot.locator(".chip").boundingBox();
  expect(chip!.x).toBeGreaterThanOrEqual(box!.x);
  expect(chip!.x + chip!.width).toBeLessThanOrEqual(box!.x + box!.width);
  await plot.press("Tab");
  await expect(plot.locator(".chip")).toHaveCount(0);
});
