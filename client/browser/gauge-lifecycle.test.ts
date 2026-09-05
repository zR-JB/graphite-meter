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
        const dial = stage.querySelector(".gauge-dial .dial-art");
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
  });
}

const nativeDialState = (page: Page) =>
  page.locator(".gauge-dial").evaluate((dial: HTMLElement) => {
    const arcs = Array.from(
      dial.querySelectorAll<SVGPathElement>(".result-arc"),
    );
    const heads = Array.from(
      dial.querySelectorAll<SVGGElement>(".head.result"),
    ).reverse();
    return {
      running: dial
        .getAnimations({ subtree: true })
        .filter((a) => a.playState === "running").length,
      arcs: arcs.map((arc, index) => {
        const matrix = new DOMMatrix(getComputedStyle(heads[index]!).transform);
        const angle = (Math.atan2(matrix.b, matrix.a) * 180) / Math.PI;
        return {
          fraction: Number.parseFloat(arc.style.strokeDasharray),
          head: ((angle - 135 + 360) % 360) / 270,
        };
      }),
    };
  });

for (const reducedMotion of [false, true]) {
  test(`completed gauge preserves arc/head geometry with ${reducedMotion ? "reduced" : "native"} motion`, async ({
    page,
  }) => {
    await page.emulateMedia({
      reducedMotion: reducedMotion ? "reduce" : "no-preference",
    });
    await openApp(page, "dummy", { width: 1024, height: 768 });
    const settings = await configureSettings(page, "active-presentation");
    await settings.getByRole("button", { name: "Close Settings" }).click();
    await page.locator(".gauge-dial").evaluate((dial: HTMLElement) => {
      dial.addEventListener("transitionrun", (event) => {
        if ((event.target as Element).classList.contains("result-layer"))
          dial.setAttribute(
            "data-reveal",
            (event as TransitionEvent).propertyName,
          );
      });
    });
    await startTest(page);
    await expect
      .poll(async () => (await nativeDialState(page)).arcs.length)
      .toBe(2);
    await expect
      .poll(async () => (await nativeDialState(page)).running)
      .toBe(0);
    expect(await page.locator(".gauge-dial").getAttribute("data-reveal")).toBe(
      reducedMotion ? null : "opacity",
    );
    const settled = await nativeDialState(page);
    for (const arc of settled.arcs)
      expect(Math.abs(arc.fraction - arc.head)).toBeLessThan(0.001);
    await page.locator(".gauge-dial").evaluate((dial: HTMLElement) => {
      dial.style.display = "none";
    });
    await page.waitForTimeout(100);
    await page.locator(".gauge-dial").evaluate((dial: HTMLElement) => {
      dial.style.display = "";
    });
    await page.waitForTimeout(50);
    expect(await nativeDialState(page)).toEqual(settled);
  });
}

test("live gauge retains its compositor surfaces across completion and restart", async ({
  page,
}) => {
  await openApp(page, "dummy", { width: 1024, height: 768 });
  const settings = await configureSettings(page, "short-600");
  await settings.getByRole("button", { name: "Close Settings" }).click();
  await page
    .locator(".gauge-dial .rotor, .gauge-dial .live-head")
    .evaluateAll((elements) => {
      for (const element of elements)
        element.setAttribute("data-retained", "true");
    });
  await startTest(page);
  await waitForCompletion(page);
  await againButton(page).click();
  await expect(page.locator('.gauge-dial [data-retained="true"]')).toHaveCount(
    3,
  );
});

async function expectJoinedSweep(page: Page) {
  const samples = await page
    .locator(".gauge-dial")
    .evaluate((dial: HTMLElement) => {
      const rotors = [
        ...dial.querySelectorAll<HTMLElement>(".rotor, .live-head"),
      ];
      const animations = rotors.map((rotor) => rotor.getAnimations()[0]);
      if (animations.some((animation) => !animation)) return [];
      for (const animation of animations) {
        const frames = (animation!.effect as KeyframeEffect).getKeyframes();
        if (frames.some((frame) => "strokeDasharray" in frame || "d" in frame))
          throw new Error("live gauge animation repaints SVG geometry");
      }
      for (const animation of animations) animation!.pause();
      const angles = (rotor: HTMLElement) => {
        const matrix = new DOMMatrix(getComputedStyle(rotor).transform);
        const angle = (Math.atan2(matrix.b, matrix.a) * 180) / Math.PI;
        return angle < -0.0001 ? angle + 360 : angle;
      };
      const values = [0, 80, 200, 400, 599].map((time) => {
        for (const animation of animations) animation!.currentTime = time;
        const [first, second, head] = rotors.map(angles);
        return {
          first: first!,
          second: second!,
          head: (head! - 135 + 360) % 360,
        };
      });
      for (const animation of animations) animation!.finish();
      return values;
    });
  expect(samples.length).toBe(5);
  for (const sample of samples) {
    expect(sample.first).toBeGreaterThanOrEqual(0);
    expect(sample.first).toBeLessThanOrEqual(180.001);
    expect(sample.second).toBeGreaterThanOrEqual(0);
    expect(sample.second).toBeLessThanOrEqual(90.001);
    expect(Math.abs(sample.first + sample.second - sample.head)).toBeLessThan(
      0.01,
    );
    if (sample.second > 0.01) expect(sample.first).toBeCloseTo(180, 2);
  }
}

test("live half-rings remain joined to the head across both directions of the halfway boundary", async ({
  page,
}) => {
  await openApp(page, "dummy", { width: 1024, height: 768 });
  const settings = await configureSettings(page, {
    "Warmup ms": "800",
    "Latency ms": "0",
    "Download ms": "10000",
    "Upload ms": "0",
  });
  await settings
    .getByText("Scale throughput automatically", { exact: true })
    .click();
  await settings.getByLabel("Maximum Mbit/s").fill("100");
  await settings.getByRole("button", { name: "Close Settings" }).click();
  await startTest(page);
  await expect
    .poll(() =>
      page
        .locator(".live-head")
        .evaluate((head: HTMLElement) =>
          Number.parseFloat(head.style.transform.slice(7)),
        ),
    )
    .toBeGreaterThan(315);
  await expectJoinedSweep(page);
  await abortButton(page).click();
  await expectJoinedSweep(page);
});

test("offscreen and reduced-motion gauges cancel native interpolation", async ({
  page,
}) => {
  await openApp(page, "dummy", { width: 1024, height: 768 });
  const settings = await configureSettings(page, {
    "Warmup ms": "800",
    "Latency ms": "0",
    "Download ms": "10000",
    "Upload ms": "0",
  });
  await settings.getByRole("button", { name: "Close Settings" }).click();
  await startTest(page);
  await expect
    .poll(() =>
      page
        .locator(".live-head")
        .evaluate((head) => head.getAnimations().length),
    )
    .toBeGreaterThan(0);
  await page.emulateMedia({ reducedMotion: "reduce" });
  await expect.poll(async () => (await nativeDialState(page)).running).toBe(0);
  await page.emulateMedia({ reducedMotion: "no-preference" });
  await page.locator(".gauge-dial").evaluate((dial: HTMLElement) => {
    dial.style.display = "none";
  });
  await page.waitForTimeout(100);
  await abortButton(page).click();
  expect((await nativeDialState(page)).running).toBe(0);
});
