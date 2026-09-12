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

test("mouse and touch tooltips dismiss when the phone workspace scrolls", async ({
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
  const workspace = page.locator(".measurement-stage");
  expect(await workspace.evaluate((node) => node.scrollTop)).toBeGreaterThan(
    80,
  );
  await term.dispatchEvent("pointerenter", { pointerType: "mouse" });
  await expect(page.getByRole("tooltip")).toContainText("RTT variation");
  await workspace.evaluate((node) => node.scrollBy(0, -40));
  await expect(page.getByRole("tooltip")).toBeHidden();
  await term.dispatchEvent("pointerup", { pointerType: "touch" });
  await expect(page.getByRole("tooltip")).toContainText("RTT variation");
  await workspace.evaluate((node) => node.scrollBy(0, -40));
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

test("tooltip inside a native diagnostic popover paints above it and leaves with its host", async ({
  page,
}) => {
  const { createServer } = await import("vite");
  const server = await createServer({
    server: { host: "127.0.0.1", port: 0 },
    logLevel: "error",
  });
  try {
    await server.listen();
    await page.setViewportSize({ width: 568, height: 480 });
    await page.goto(server.resolvedUrls!.local[0]);
    await page.emulateMedia({ reducedMotion: "reduce" });
    await page.evaluate(async () => {
      const path = "/src/lib/actions/tooltip.ts";
      const { tooltip } = await import(path);
      const host = document.createElement("div");
      host.id = "diagnostic-host";
      host.popover = "auto";
      host.style.cssText =
        "position:fixed;inset:100px auto auto 30px;margin:0;width:300px;height:200px;background:white;padding:80px 20px;box-sizing:border-box";
      const term = document.createElement("span");
      term.textContent = "Partial accounting";
      host.append(term);
      document.body.append(host);
      tooltip(term, {
        text: "Some probe outcomes are unknown and remain separate from timed-out replies.",
        instant: true,
      });
      host.showPopover();
    });
    await page.locator("#diagnostic-host span").hover();
    const tip = page.getByRole("tooltip");
    await expect(tip).toContainText("Some probe outcomes are unknown");
    expect(
      await tip.evaluate((element) => {
        const rect = element.getBoundingClientRect();
        element.style.pointerEvents = "auto";
        const top = document.elementFromPoint(
          rect.x + rect.width / 2,
          rect.y + rect.height / 2,
        );
        element.style.pointerEvents = "none";
        return top === element;
      }),
    ).toBe(true);
    const cdp = await page.context.newCDPSession(page);
    await cdp.send("Emulation.setPageScaleFactor", { pageScaleFactor: 2 });
    await expect(tip).toBeHidden();
    await page
      .locator("#diagnostic-host span")
      .evaluate((node) =>
        node.dispatchEvent(
          new PointerEvent("pointerenter", { pointerType: "mouse" }),
        ),
      );
    await expect(tip).toBeVisible();
    expect(
      await tip.evaluate((node) => {
        const box = node.getBoundingClientRect();
        const viewport = visualViewport!;
        return (
          box.left >= viewport.offsetLeft &&
          box.top >= viewport.offsetTop &&
          box.right <= viewport.offsetLeft + viewport.width &&
          box.bottom <= viewport.offsetTop + viewport.height
        );
      }),
    ).toBe(true);
    await page.evaluate(() =>
      document.getElementById("diagnostic-host")!.hidePopover(),
    );
    await expect(tip).toBeHidden();
  } finally {
    await server.close();
  }
});
