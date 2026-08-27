import {
  expect,
  openApp,
  prepareApp,
  startTest,
  test,
  waitForCompletion,
} from "./webview";
async function gaugeHeight(page: import("./webview").Page) {
  return page
    .locator(".gauge-panel .stage")
    .evaluate((element) => element.getBoundingClientRect().height);
}
test("portrait phone gauge height is stable across live and result content", async ({
  page,
}) => {
  await openApp(page, "dummy", { width: 390, height: 844 });
  const settings = await prepareApp(page, "mobile-scroll", "dummy", {
    width: 390,
    height: 844,
  });
  await settings.getByRole("button", { name: "Close Settings" }).click();
  const idle = await gaugeHeight(page);
  await startTest(page);
  await expect(page.locator(".result-chip")).toHaveCount(3, {
    timeout: 10_000,
  });
  await page.evaluate(() => scrollTo(0, document.documentElement.scrollHeight));
  const live = await gaugeHeight(page);
  expect(Math.abs(live - idle)).toBeLessThanOrEqual(1);
  await waitForCompletion(page, 10_000);
  await page.evaluate(() => scrollTo(0, document.documentElement.scrollHeight));
  const complete = await gaugeHeight(page);
  expect(Math.abs(complete - idle)).toBeLessThanOrEqual(1);
  // Resetting to the short, latency-free document must not leave a trailing root/body scroll area below the footer.
  await page
    .getByRole("button", {
      name: "Graphite Meter — return to a fresh, blank test",
    })
    .click();
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
