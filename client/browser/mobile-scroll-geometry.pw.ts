import { expect, test } from "@playwright/test";

async function gaugeHeight(page: import("@playwright/test").Page) {
  return page
    .locator(".gauge-panel .stage")
    .evaluate((element) => element.getBoundingClientRect().height);
}

test("portrait phone gauge height is stable across live and result content", async ({
  page,
}) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await page.goto("/?engine=dummy");

  await page.getByRole("button", { name: "Open settings" }).click();
  const settings = page.locator('[aria-label="Settings"]');
  await settings.getByRole("button", { name: "custom" }).click();
  for (const [label, value] of [
    ["Warmup ms", "0"],
    ["Latency ms", "700"],
    ["Download ms", "700"],
    ["Upload ms", "2600"],
  ] as const)
    await settings.getByLabel(label).fill(value);
  await settings.getByRole("button", { name: "Close Settings" }).click();

  const idle = await gaugeHeight(page);
  await page.getByRole("button", { name: "Start the speed test" }).click();

  await expect(page.locator(".result-chip")).toHaveCount(3, {
    timeout: 10_000,
  });
  const live = await gaugeHeight(page);
  expect(Math.abs(live - idle)).toBeLessThanOrEqual(1);

  await expect(
    page.getByRole("button", { name: "Run the test again" }),
  ).toBeVisible({ timeout: 10_000 });
  const complete = await gaugeHeight(page);
  expect(Math.abs(complete - idle)).toBeLessThanOrEqual(1);

  // Resetting to the short, latency-free document must not leave a trailing
  // root/body scroll area below the application's own footer.
  await page.getByRole("button", {
    name: "Graphite Meter — return to a fresh, blank test",
  }).click();
  await page.getByRole("switch", { name: "Latency stage" }).click();
  const reset = await gaugeHeight(page);
  expect(Math.abs(reset - idle)).toBeLessThanOrEqual(1);

  const tailGap = await page.evaluate(() => {
    const consoleEl = document.querySelector("#console");
    if (!(consoleEl instanceof HTMLElement)) throw new Error("missing console");
    const consoleBottom = consoleEl.getBoundingClientRect().bottom + scrollY;
    return document.documentElement.scrollHeight - consoleBottom;
  });
  expect(Math.abs(tailGap)).toBeLessThanOrEqual(1);
});
