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
