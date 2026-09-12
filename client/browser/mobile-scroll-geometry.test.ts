import {
  expect,
  prepareApp,
  startTest,
  test,
  waitForCompletion,
} from "./webview";
async function anchoredLayout(page: import("./webview").Page) {
  await page.locator(".measurement-stage").evaluate((node) => {
    node.scrollTop = node.scrollHeight;
  });
  const layout = await page.evaluate(() => {
    const footer = document
      .querySelector("footer.status")!
      .getBoundingClientRect();
    const chart = document.querySelector(".chart")!.getBoundingClientRect();
    return {
      footerBottom: footer.bottom,
      viewportBottom: innerHeight,
      chartBottom: chart.bottom,
      footerTop: footer.top,
      rootOverflow: document.documentElement.scrollHeight - innerHeight,
      labelsFit: [...document.querySelectorAll(".seg")].every((card) => {
        const box = card.getBoundingClientRect();
        return [...card.querySelectorAll(".seg-label, .seg-tag")].every(
          (label) => {
            const r = label.getBoundingClientRect();
            return (
              r.left >= box.left &&
              r.right <= box.right &&
              r.bottom <= box.bottom
            );
          },
        );
      }),
      statusFits: [...document.querySelectorAll("footer.status > span")].every(
        (node) => {
          const box = node.getBoundingClientRect();
          return (
            box.width === 0 ||
            (box.left >= footer.left &&
              box.right <= footer.right &&
              box.bottom <= footer.bottom)
          );
        },
      ),
    };
  });
  expect(
    Math.abs(layout.footerBottom - layout.viewportBottom),
  ).toBeLessThanOrEqual(1);
  expect(layout.chartBottom).toBeLessThanOrEqual(layout.footerTop);
  expect(layout.rootOverflow).toBeLessThanOrEqual(1);
  expect(layout.labelsFit).toBe(true);
  expect(layout.statusFits).toBe(true);
}
async function gaugeHeight(page: import("./webview").Page) {
  return page
    .locator(".gauge-panel .stage")
    .evaluate((element) => element.getBoundingClientRect().height);
}
test("portrait phone gauge height is stable across live and result content", async ({
  page,
}) => {
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
  await anchoredLayout(page);
  await page.locator(".stage-track").scrollIntoViewIfNeeded();
  await page.artifact("mobile-compact-stage-live");
  const live = await gaugeHeight(page);
  expect(Math.abs(live - idle)).toBeLessThanOrEqual(1);
  await waitForCompletion(page, 10_000);
  await anchoredLayout(page);
  await page.locator(".stage-track").scrollIntoViewIfNeeded();
  await page.artifact("mobile-compact-stage-complete");
  const complete = await gaugeHeight(page);
  expect(Math.abs(complete - idle)).toBeLessThanOrEqual(1);
  for (const width of [320, 390, 430, 520, 640, 700]) {
    await page.setViewportSize({ width, height: 844 });
    await anchoredLayout(page);
    await page.artifact(`mobile-results-${width}`);
  }
  await page.setViewportSize({ width: 390, height: 844 });
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
