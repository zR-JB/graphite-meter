import {
  expect,
  expectNoHorizontalOverflow,
  openApp,
  openSettings,
  openEndpointInfo,
  test,
} from "./webview";

type Page = Parameters<typeof openApp>[0];
async function viewportSize(
  page: Page,
  size: { width: number; height: number },
) {
  await page.setViewportSize(size);
  const cdp = await page.context.newCDPSession(page);
  await cdp.send("Emulation.setDeviceMetricsOverride", {
    ...size,
    deviceScaleFactor: 1,
    mobile: false,
  });
}
const geometry = (page: Page) =>
  page.evaluate(() => {
    const rect = (selector: string) =>
      document.querySelector(selector)!.getBoundingClientRect().width;
    const panels = [
      ...document.querySelectorAll<HTMLElement>(
        ".panel-layer.docked.open > .panel",
      ),
    ];
    return {
      stage: rect(".measurement-stage"),
      widths: panels.map((panel) => panel.getBoundingClientRect().width),
      values: panels.map((panel) =>
        Number(
          panel.querySelector("[role=slider]")!.getAttribute("aria-valuenow"),
        ),
      ),
      maxima: panels.map((panel) =>
        Number(
          panel.querySelector("[role=slider]")!.getAttribute("aria-valuemax"),
        ),
      ),
    };
  });

for (const viewport of [
  { width: 1200, height: 800 },
  { width: 1280, height: 720 },
  { width: 1440, height: 900 },
]) {
  test(`large saved docks preserve the instrument stage at ${viewport.width}`, async ({
    page,
  }) => {
    await openApp(page, "dummy", viewport);
    await viewportSize(page, viewport);
    await page.evaluate(() => {
      localStorage.setItem(
        "graphite-meter:v1",
        JSON.stringify({
          ...JSON.parse(localStorage.getItem("graphite-meter:v1") ?? "{}"),
          dockWidth: { left: 720, right: 720 },
        }),
      );
    });
    await page.reload();
    await openSettings(page);
    await openEndpointInfo(page);
    await expect
      .poll(async () => (await geometry(page)).widths.length)
      .toBe(viewport.width < 1440 ? 1 : 2);
    const actual = await geometry(page);
    expect(actual.stage).toBeGreaterThanOrEqual(799);
    expect(actual.widths.every((width) => width >= 320 && width <= 720)).toBe(
      true,
    );
    expect(actual.values).toEqual(actual.widths);
    expect(actual.maxima.every((max, i) => max >= actual.values[i]!)).toBe(
      true,
    );
    expect(
      await page.evaluate(
        () => JSON.parse(localStorage.getItem("graphite-meter:v1")!).dockWidth,
      ),
    ).toEqual({ left: 720, right: 720 });
    await expectNoHorizontalOverflow(page.locator(".measurement-stage"));
    await page.artifact(`docks-${viewport.width}x${viewport.height}`);
    if (viewport.width >= 1440) {
      const handle = page.getByRole("slider", {
        name: /Resize Settings panel/,
      });
      await viewportSize(page, { width: 1600, height: 900 });
      await expect.poll(async () => (await geometry(page)).widths[0]).toBe(400);
      const initial = 400;
      await handle.press("ArrowLeft");
      await expect
        .poll(async () => (await geometry(page)).widths[0])
        .toBe(initial - 16);
      await viewportSize(page, { width: 1100, height: 700 });
      await expect(page.getByRole("slider")).toHaveCount(0);
      await expect(
        page.getByRole("dialog", { name: "Endpoint info" }),
      ).toBeVisible();
      await expectNoHorizontalOverflow(page.locator("#console"));
      await page.artifact("docks-tablet-landscape");
    }
  });
}

for (const interruption of [
  "release",
  "capture",
  "close",
  "breakpoint",
] as const) {
  test(`dock drag restores body styles on ${interruption}`, async ({
    page,
  }) => {
    await openApp(page, "dummy", { width: 1440, height: 900 });
    await openSettings(page);
    await page.evaluate(() => {
      document.body.style.cursor = "crosshair";
      document.body.style.userSelect = "text";
    });
    const handle = page.getByRole("slider", { name: /Resize Settings panel/ });
    const point = await handle.evaluate((node) => {
      const r = node.getBoundingClientRect();
      return { x: r.x + r.width / 2, y: r.y + 100 };
    });
    const cdp = await page.context.newCDPSession(page);
    await cdp.send("Input.dispatchMouseEvent", {
      type: "mousePressed",
      ...point,
      button: "left",
      buttons: 1,
      clickCount: 1,
    });
    await expect
      .poll(() => page.evaluate(() => document.body.style.cursor))
      .toBe("col-resize");
    await cdp.send("Input.dispatchMouseEvent", {
      type: "mouseMoved",
      x: point.x + 32,
      y: point.y,
      button: "left",
      buttons: 1,
    });
    await expect.poll(async () => (await geometry(page)).widths[0]).toBe(432);
    if (interruption === "capture")
      await handle.evaluate((node) => {
        const handle = node as HTMLElement;
        if (handle.hasPointerCapture(1)) handle.releasePointerCapture(1);
      });
    if (interruption === "close") await page.keyboard.press("Escape");
    if (interruption === "breakpoint")
      await viewportSize(page, { width: 1100, height: 700 });
    if (interruption !== "release")
      await expect
        .poll(() => page.evaluate(() => document.body.style.cursor))
        .toBe("crosshair");
    await cdp.send("Input.dispatchMouseEvent", {
      type: "mouseReleased",
      ...point,
      button: "left",
      buttons: 0,
      clickCount: 1,
    });
    await expect
      .poll(() => page.evaluate(() => document.body.style.cursor))
      .toBe("crosshair");
    expect(await page.evaluate(() => document.body.style.userSelect)).toBe(
      "text",
    );
    if (interruption === "breakpoint") {
      await page.keyboard.press("Escape");
      await expect
        .poll(() => page.evaluate(() => document.body.style.cursor))
        .toBe("crosshair");
    }
  });
}
