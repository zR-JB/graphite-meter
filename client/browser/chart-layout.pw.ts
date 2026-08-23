import { expect, test } from "@playwright/test";

test("chart axes and time ticks are DOM labels anchored inside the canvas layout", async ({
  page,
}) => {
  await page.goto("/?engine=dummy");
  await page.getByRole("button", { name: "Start the speed test" }).click();

  const plot = page.locator(
    '[role="img"][aria-label="Throughput and latency over time"]',
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
  browserName,
}) => {
  test.skip(browserName !== "chromium", "CDP page scale is Chromium-only");
  await page.goto("/?engine=dummy");

  const ratios = () =>
    page.locator("canvas").evaluateAll((canvases) =>
      canvases.map((canvas) => ({
        ratio: canvas.width / canvas.getBoundingClientRect().width,
        width: canvas.getBoundingClientRect().width,
      })),
    );
  const before = await ratios();
  const session = await page.context().newCDPSession(page);
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
