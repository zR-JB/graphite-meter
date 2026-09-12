import {
  expect,
  openApp,
  openSettings,
  prepareApp,
  startAndWait,
  test,
} from "./webview";

test("keyboard tooltip survives focus reveal but dismisses on later scroll", async ({
  page,
}) => {
  await page.emulateMedia({ reducedMotion: "reduce" });
  await openApp(page, "dummy", { width: 390, height: 844 });
  const settings = await openSettings(page);
  await page.keyboard.press("Tab");
  const bits = settings.getByRole("button", { name: "Bits", exact: true });
  const scrollport = settings.locator(".panel-body");
  const before = await scrollport.evaluate((node) => node.scrollTop);
  expect((await bits.boundingBox())!.y).toBeGreaterThan(844);
  await bits.focus();
  await page.evaluate(
    () =>
      new Promise<void>((resolve) =>
        requestAnimationFrame(() => requestAnimationFrame(() => resolve())),
      ),
  );
  expect(await scrollport.evaluate((node) => node.scrollTop)).toBeGreaterThan(
    before,
  );
  await expect(bits).toBeFocused();
  await expect(page.getByRole("tooltip")).toContainText("Bits per second");

  await scrollport.evaluate((node) => {
    node.scrollTop += 40;
  });
  await expect(page.getByRole("tooltip")).toBeHidden();
  await expect(bits).toBeFocused();
});

test("blur cancels a keyboard tooltip waiting for the focus reveal frame", async ({
  page,
}) => {
  await page.emulateMedia({ reducedMotion: "reduce" });
  await openApp(page);
  const settings = await openSettings(page);
  await page.keyboard.press("Tab");
  await settings
    .getByRole("button", { name: "Bits", exact: true })
    .evaluate((node) => {
      node.focus();
      node.blur();
    });
  await page.evaluate(
    () =>
      new Promise<void>((resolve) =>
        requestAnimationFrame(() => requestAnimationFrame(() => resolve())),
      ),
  );
  await expect(page.getByRole("tooltip")).toBeHidden();
});

test("mouse and touch tooltips still dismiss when the page scrolls", async ({
  page,
}) => {
  await page.emulateMedia({ reducedMotion: "reduce" });
  const settings = await prepareApp(
    page,
    {
      "Warmup ms": "0",
      "Latency ms": "900",
      "Download ms": "900",
      "Upload ms": "0",
    },
    "dummy",
    { width: 390, height: 844 },
  );
  await settings.getByRole("button", { name: "Close Settings" }).click();
  await startAndWait(page);
  const term = page.locator(".result-card .jitter-term");
  await term.scrollIntoViewIfNeeded();
  expect(await page.evaluate(() => scrollY)).toBeGreaterThan(80);
  await term.dispatchEvent("pointerenter", { pointerType: "mouse" });
  await expect(page.getByRole("tooltip")).toContainText("RTT variation");
  await page.evaluate(() => window.scrollBy(0, -40));
  await expect(page.getByRole("tooltip")).toBeHidden();
  await term.dispatchEvent("pointerup", { pointerType: "touch" });
  await expect(page.getByRole("tooltip")).toContainText("RTT variation");
  await page.evaluate(() => window.scrollBy(0, -40));
  await expect(page.getByRole("tooltip")).toBeHidden();
});

test("tooltip dismissal clears accessible state immediately and finishes its CSS fade", async ({
  page,
}) => {
  await page.emulateMedia({ reducedMotion: "no-preference" });
  await openApp(page);
  const settings = await openSettings(page);
  const bits = settings.getByRole("button", { name: "Bits", exact: true });
  await bits.hover();
  await expect(page.getByRole("tooltip")).toContainText("Bits per second");
  await page.locator(".gm-tooltip").evaluate(async (node: HTMLElement) => {
    await Promise.allSettled(
      node.getAnimations().map((animation) => animation.finished),
    );
  });
  const state = await bits.evaluate((node) => {
    node.dispatchEvent(
      new PointerEvent("pointerleave", { pointerType: "mouse" }),
    );
    const leaving = document.querySelector<HTMLElement>(".gm-tooltip");
    return {
      described: node.hasAttribute("aria-describedby"),
      role: leaving?.getAttribute("role"),
      hidden: leaving?.getAttribute("aria-hidden"),
      animating: !!leaving?.getAnimations().length,
    };
  });
  expect(state).toEqual({
    described: false,
    role: null,
    hidden: "true",
    animating: true,
  });
  await expect(page.locator(".gm-tooltip")).toHaveCount(0);
});
