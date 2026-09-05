import {
  abortButton,
  againButton,
  configureSettings,
  expect,
  gaugeStage,
  openApp,
  startTest,
  test,
  waitForCompletion,
  type Page,
} from "./webview";
async function expectCoherentGauge(page: Page): Promise<void> {
  await expect(gaugeStage(page).locator(".gauge-dial")).toBeVisible();
  await expect
    .poll(() =>
      gaugeStage(page).evaluate((stage) => {
        const dial = stage.querySelector(".gauge-dial");
        if (!(dial instanceof SVGSVGElement)) return false;
        const stageBox = stage.getBoundingClientRect();
        const box = dial.getBoundingClientRect();
        const viewBox = dial.viewBox.baseVal;
        const painted = Array.from(dial.querySelectorAll("path")).filter(
          (path) =>
            getComputedStyle(path).stroke !== "none" &&
            path.getTotalLength() > 0,
        );
        const bounds = dial.getBBox();
        return (
          Math.abs(stageBox.width - box.width) <= 2 &&
          Math.abs(stageBox.height - box.height) <= 2 &&
          Math.abs(viewBox.width - box.width) <= 1 &&
          Math.abs(viewBox.height - box.height) <= 1 &&
          painted.length >= 10 &&
          bounds.x >= 0 &&
          bounds.y >= 0 &&
          bounds.x + bounds.width <= viewBox.width &&
          bounds.y + bounds.height <= viewBox.height
        );
      }),
    )
    .toBe(true);
}
for (const viewport of [
  { width: 1024, height: 768, label: "desktop" },
  { width: 390, height: 640, label: "mobile" },
]) {
  test(`gauge viewBox and painted geometry survive ${viewport.label} lifecycle churn`, async ({
    page,
  }) => {
    const errors: string[] = [];
    page.on("pageerror", (error) => errors.push(error.message));
    await openApp(page, "dummy", viewport);
    await expectCoherentGauge(page);
    await page.setViewportSize({
      width: viewport.width,
      height: viewport.height + 120,
    });
    await expectCoherentGauge(page);
    const session = await page.context.newCDPSession(page);
    await session.send("Emulation.setDeviceMetricsOverride", {
      width: viewport.width,
      height: viewport.height + 120,
      deviceScaleFactor: 3,
      mobile: false,
    });
    await expectCoherentGauge(page);
    await gaugeStage(page).evaluate((stage) => {
      stage.setAttribute("data-test-hidden", "true");
      (stage as HTMLElement).style.display = "none";
    });
    await page.waitForTimeout(80);
    await gaugeStage(page).evaluate((stage) => {
      stage.removeAttribute("data-test-hidden");
      (stage as HTMLElement).style.removeProperty("display");
    });
    await expectCoherentGauge(page);
    const settings = await configureSettings(page, "lifecycle");
    await settings.getByRole("button", { name: "Close Settings" }).click();
    await startTest(page);
    await expect(abortButton(page)).toBeVisible();
    await expectCoherentGauge(page);
    await abortButton(page).click();
    await expect(againButton(page)).toBeVisible();
    await expectCoherentGauge(page);
    await againButton(page).click();
    await expect(abortButton(page)).toBeVisible();
    await expectCoherentGauge(page);
    expect(errors).toEqual([]);
  });
}

const nativeDialState = (page: Page) =>
  page.locator(".gauge-dial").evaluate((dial: SVGSVGElement) => {
    const arcs = Array.from(dial.querySelectorAll<SVGPathElement>(".reveal"));
    const heads = Array.from(
      dial.querySelectorAll<SVGGElement>(".head.result"),
    ).reverse();
    return {
      running: dial
        .getAnimations({ subtree: true })
        .filter((animation) => animation.playState === "running").length,
      arcs: arcs.map((arc, index) => {
        const matrix = new DOMMatrix(getComputedStyle(heads[index]!).transform);
        const angle = (Math.atan2(matrix.b, matrix.a) * 180) / Math.PI;
        return {
          current: Number.parseFloat(getComputedStyle(arc).strokeDasharray),
          target: Number.parseFloat(arc.style.strokeDasharray),
          head: ((angle - 135 + 360) % 360) / 270,
        };
      }),
    };
  });

for (const reducedMotion of [false, true]) {
  test(`native result motion ${reducedMotion ? "respects reduced motion" : "reveals once with synchronized heads"}`, async ({
    page,
  }) => {
    await page.emulateMedia({
      reducedMotion: reducedMotion ? "reduce" : "no-preference",
    });
    await openApp(page, "dummy", { width: 1024, height: 768 });
    const settings = await configureSettings(page, "active-presentation");
    await settings.getByRole("button", { name: "Close Settings" }).click();
    await startTest(page);
    await expect
      .poll(async () => (await nativeDialState(page)).arcs.length)
      .toBe(2);
    const reveal = await nativeDialState(page);
    if (reducedMotion) expect(reveal.running).toBe(0);
    else {
      expect(reveal.running).toBe(4);
      for (const arc of reveal.arcs) {
        expect(arc.current).toBeLessThan(arc.target);
        expect(Math.abs(arc.current - arc.head)).toBeLessThan(0.01);
      }
    }
    await expect
      .poll(async () => (await nativeDialState(page)).running)
      .toBe(0);
    const settled = await nativeDialState(page);
    for (const arc of settled.arcs) {
      expect(arc.current).toBe(arc.target);
      expect(Math.abs(arc.current - arc.head)).toBeLessThan(0.01);
    }
    await page.locator(".gauge-dial").evaluate((dial: SVGSVGElement) => {
      dial.style.display = "none";
    });
    await page.waitForTimeout(100);
    await page.locator(".gauge-dial").evaluate((dial: SVGSVGElement) => {
      dial.style.display = "";
    });
    await page.waitForTimeout(50);
    expect(await nativeDialState(page)).toEqual(settled);
  });
}

test("live gauge primitives retain their position across a completed run and restart", async ({
  page,
}) => {
  await openApp(page, "dummy", { width: 1024, height: 768 });
  const settings = await configureSettings(page, "short-600");
  await settings.getByRole("button", { name: "Close Settings" }).click();
  await page
    .locator(".gauge-dial .sweep, .gauge-dial .live .head")
    .evaluateAll((elements) => {
      for (const element of elements)
        element.setAttribute("data-retained", "true");
    });
  await startTest(page);
  await waitForCompletion(page);
  await page.waitForTimeout(650);
  await againButton(page).click();
  await expect(page.locator('.gauge-dial [data-retained="true"]')).toHaveCount(
    2,
  );
  await expect
    .poll(() =>
      page.locator(".gauge-dial").evaluate((dial) => {
        const arc = dial.querySelector(".sweep") as SVGPathElement;
        const head = dial.querySelector(".live .head") as SVGGElement;
        const current = Number.parseFloat(
          getComputedStyle(arc).strokeDasharray,
        );
        const target = Number.parseFloat(arc.style.strokeDasharray);
        const matrix = new DOMMatrix(getComputedStyle(head).transform);
        const angle = (Math.atan2(matrix.b, matrix.a) * 180) / Math.PI;
        const headFraction = ((angle - 135 + 360) % 360) / 270;
        return (
          getComputedStyle(arc).opacity === "1" &&
          Math.abs(current - target) > 0.001 &&
          Math.abs(current - headFraction) < 0.001
        );
      }),
    )
    .toBe(true);
});

test("live gauge blends phase color and responds promptly to an interrupted transfer", async ({
  page,
}) => {
  await page.emulateMedia({ reducedMotion: "no-preference" });
  await openApp(page, "dummy", { width: 1024, height: 768 });
  const settings = await configureSettings(page, {
    "Warmup ms": "800",
    "Latency ms": "0",
    "Download ms": "1800",
    "Upload ms": "0",
  });
  await settings.getByRole("button", { name: "Close Settings" }).click();
  const sweep = page.locator(".gauge-dial .sweep");
  // Sample native transition time in the browser, independent of CI scheduling delay.
  await sweep.evaluate((arc: SVGPathElement) => {
    arc.addEventListener("transitionrun", (event) => {
      if ((event as TransitionEvent).propertyName !== "stroke") return;
      if (arc.hasAttribute("data-color-motion")) return;
      const animation = arc
        .getAnimations()
        .find(
          (item) =>
            item instanceof CSSTransition &&
            item.transitionProperty === "stroke",
        );
      if (!animation) return;
      animation.pause();
      animation.currentTime = 0;
      const initial = getComputedStyle(arc).stroke;
      animation.currentTime = 90;
      const middle = getComputedStyle(arc).stroke;
      animation.finish();
      arc.setAttribute(
        "data-color-motion",
        JSON.stringify([initial, middle, getComputedStyle(arc).stroke]),
      );
    });
  });
  await startTest(page);
  await expect
    .poll(() => sweep.getAttribute("data-color-motion"))
    .not.toBeNull();
  const [idleColor, enteringColor, warmupColor] = JSON.parse(
    (await sweep.getAttribute("data-color-motion"))!,
  );
  expect(enteringColor).not.toBe(idleColor);
  expect(enteringColor).not.toBe(warmupColor);
  await expect
    .poll(() =>
      sweep.evaluate(
        (arc) =>
          arc.getAttribute("stroke") === "var(--phase-download)" &&
          Number.parseFloat(getComputedStyle(arc).strokeDasharray) > 0.3,
      ),
    )
    .toBe(true);
  await sweep.evaluate((arc: SVGPathElement) => {
    arc.addEventListener("transitionrun", (event) => {
      if ((event as TransitionEvent).propertyName !== "stroke-dasharray")
        return;
      const target = Number.parseFloat(arc.style.strokeDasharray);
      if (target !== 0.05) return;
      const animation = arc
        .getAnimations()
        .find(
          (item) =>
            item instanceof CSSTransition &&
            item.transitionProperty === "stroke-dasharray",
        );
      if (!animation) return;
      animation.pause();
      animation.currentTime = 0;
      const before = Number.parseFloat(getComputedStyle(arc).strokeDasharray);
      animation.currentTime = 100;
      const current = Number.parseFloat(getComputedStyle(arc).strokeDasharray);
      animation.finish();
      const settled = Number.parseFloat(getComputedStyle(arc).strokeDasharray);
      arc.setAttribute(
        "data-fall-motion",
        JSON.stringify({ before, current, target, settled }),
      );
    });
  });
  await abortButton(page).click();
  await expect
    .poll(() => sweep.getAttribute("data-fall-motion"))
    .not.toBeNull();
  const falling = JSON.parse((await sweep.getAttribute("data-fall-motion"))!);
  expect(falling.target).toBe(0.05);
  expect(falling.current).toBeLessThan(falling.before);
  expect(falling.current).toBeGreaterThan(falling.target);
  expect(falling.current - falling.target).toBeLessThan(
    (falling.before - falling.target) * 0.5,
  );
  expect(falling.settled).toBe(0.05);
});
