import {
  expect,
  expectVisible,
  prepareApp,
  openSettings,
  resultCards,
  startAndWait,
  startTest,
  test,
  waitForCompletion,
} from "./webview";
test("default wire estimates stay out of live measurement and concise after completion", async ({
  page,
}) => {
  await prepareApp(page, "short");
  await expect(page.getByLabel("Show estimated wire rate")).toBeChecked();
  await startTest(page);
  await expect(page.locator(".gauge-value")).not.toHaveText("—");
  await expect(page.locator(".metric-wrap")).not.toContainText(/wire/i);
  await expect(page.locator(".result-chip")).not.toContainText(/wire/i);
  await expect(page.locator(".result-card .est")).toHaveCount(0);
  await waitForCompletion(page);
  const estimate = resultCards(page).locator(".est");
  await expect(estimate).toContainText("wire +");
  await expect(estimate).not.toContainText("wire estimate");
  await expect(estimate.locator(".est-num")).toHaveText(/\d/);
  await expect(estimate).not.toContainText("n/a");
});
test("a persisted opt-out hides only wire-estimate presentation", async ({
  page,
}) => {
  await prepareApp(page, "short");
  await page.getByText("Show estimated wire rate", { exact: true }).click();
  await expect(page.getByLabel("Show estimated wire rate")).not.toBeChecked();
  await expect
    .poll(() =>
      page.evaluate(() => {
        const raw = localStorage.getItem("graphite-meter:v1");
        return raw ? JSON.parse(raw).showWireEstimates : null;
      }),
    )
    .toBe(false);
  await page.reload();
  await openSettings(page);
  await expect(page.getByLabel("Show estimated wire rate")).not.toBeChecked();
  await startTest(page);
  await expect(page.locator(".gauge-value")).not.toHaveText("—");
  await waitForCompletion(page);
  await expect(resultCards(page)).toHaveCount(1);
  await expect(page.locator(".result-card .est")).toHaveCount(0);
});
test("result wire details work with mouse, keyboard, touch, and narrow viewports", async ({
  page,
}) => {
  const settings = await prepareApp(page, "short", "dummy", {
    width: 360,
    height: 740,
  });
  await settings.getByRole("button", { name: "Close Settings" }).click();
  await startAndWait(page);
  const tag = resultCards(page).locator(".est-tag");
  await expect(tag).toHaveCSS("text-decoration-line", "underline");
  await expect(tag).toHaveCSS("text-decoration-style", "dotted");
  await tag.hover();
  const tooltip = page.getByRole("tooltip");
  await expect(tooltip).toContainText("Estimated Ethernet overhead:");
  await expect(tooltip).toContainText("(assumed)");
  await expect(tooltip).toContainText(/IP: IPv[46]/);
  await expect(tooltip).toContainText("Transport:");
  await expect(tooltip).toHaveCSS("white-space", "pre-line");
  const box = await tooltip.boundingBox();
  expect(box).not.toBeNull();
  expect(box!.x).toBeGreaterThanOrEqual(0);
  expect(box!.x + box!.width).toBeLessThanOrEqual(360);
  await page.mouse.move(0, 0);
  await expect(tooltip).toHaveCount(0);
  await tag.focus();
  await page.keyboard.press("Tab");
  await page.keyboard.press("Shift+Tab");
  await expectVisible(page.getByRole("tooltip"));
  await page.keyboard.press("Escape");
  await expect(page.getByRole("tooltip")).toHaveCount(0);
  await tag.dispatchEvent("pointerup", { pointerType: "touch" });
  await expectVisible(page.getByRole("tooltip"));
  await tag.dispatchEvent("pointerup", { pointerType: "touch" });
  await expect(page.getByRole("tooltip")).toHaveCount(0);
});

test("four-stage result details survive responsive and wire-preference changes", async ({
  page,
}) => {
  const settings = await prepareApp(
    page,
    {
      "Warmup ms": "0",
      "Latency ms": "900",
      "Download ms": "900",
      "Upload ms": "900",
    },
    "dummy",
    { width: 1440, height: 900 },
  );
  await settings
    .locator("label.switch", {
      hasText: "Include concurrent download + upload",
    })
    .click();
  await settings.getByLabel("Bidirectional ms").fill("900");
  await settings.getByRole("button", { name: "Close Settings" }).click();
  await startAndWait(page);
  const bidirectional = resultCards(page).filter({ hasText: "Bi-dir" });
  await expect(bidirectional.locator(".est")).toContainText("wire +");
  await bidirectional.locator(".est-tag").hover();
  await expect(page.getByRole("tooltip")).toContainText(
    "Estimated Ethernet overhead:",
  );
  const jitter = page.locator(".result-card .jitter");
  await expect(jitter).toHaveText("0.0 ms jitter");
  await expect(
    page.locator(".terminal-readout.download .terminal-number"),
  ).toHaveText("320.0");
  const geometry = () =>
    resultCards(page).evaluateAll((cards) =>
      cards.map((card) => {
        const sub = card.querySelector(".sub")?.getBoundingClientRect();
        const readout = card
          .querySelector(".result-readout")!
          .getBoundingClientRect();
        const heading = [...card.querySelector("header")!.children].map(
          (child) => child.getBoundingClientRect(),
        );
        return {
          groupsSeparated:
            Math.max(...heading.map((box) => box.bottom)) + 5 <= readout.top,
          height: card.getBoundingClientRect().height,
          overflow: card.scrollWidth > card.clientWidth,
          detailBelowReadout: !sub || sub.top >= readout.bottom + 5,
          jitterAlignment: (() => {
            const jitter = card
              .querySelector(".jitter-num")
              ?.getBoundingClientRect();
            const primary = card
              .querySelector(".val .num")!
              .getBoundingClientRect();
            return jitter ? Math.abs(jitter.left - primary.left) : 0;
          })(),
        };
      }),
    );
  // The instrument can be wide while each of its four cards is narrow.
  // Check the actual heading content, which can collide without scroll overflow.
  for (const width of [1024, 1280, 1440]) {
    await page.setViewportSize({ width, height: 900 });
    await expect
      .poll(async () =>
        (await geometry()).every((card) => card.groupsSeparated),
      )
      .toBe(true);
  }
  const before = await geometry();
  expect(before).toHaveLength(4);
  expect(
    before.every(
      (card) =>
        !card.overflow && card.detailBelowReadout && card.jitterAlignment <= 1,
    ),
  ).toBe(true);
  const preferences = await openSettings(page);
  await preferences
    .getByText("Show estimated wire rate", { exact: true })
    .click();
  await preferences.getByRole("button", { name: "Close Settings" }).click();
  await expect(page.locator(".result-card .est")).toHaveCount(0);
  await expect(jitter).toHaveText("0.0 ms jitter");
  await expect(page.locator(".result-card .sub")).toContainText(
    "↓ 320.0 ↑ 64.00",
  );
  const after = await geometry();
  expect(
    after.every(
      (card, index) =>
        card.height <= before[index]!.height &&
        card.groupsSeparated &&
        card.detailBelowReadout &&
        !card.overflow,
    ),
  ).toBe(true);
  for (const [width, height] of [
    [1440, 900],
    [390, 844],
    [320, 740],
  ]) {
    await page.setViewportSize({ width, height });
    await page.locator(".result-cards").scrollIntoViewIfNeeded();
    await page.evaluate(
      () =>
        new Promise<void>((resolve) =>
          requestAnimationFrame(() => requestAnimationFrame(() => resolve())),
        ),
    );
    await expect(page.locator(".stat-label")).toHaveCount(4);
  }
});
