import { expect, prepareApp, startAndWait, test, type Page } from "./webview";

async function completedProfile(page: Page) {
  const settings = await prepareApp(page, "latency-only", "dummy", {
    width: 1440,
    height: 900,
  });
  await settings.getByRole("button", { name: "Close Settings" }).click();
  await startAndWait(page);
  const profile = page.locator(".live-profile [data-latency-profile]");
  await expect(profile).toHaveAttribute("data-motion", "true");
  return profile;
}

// Exercise the browser's retargeting boundary on the actual profile artwork.
// Measurement values and hover text remain owned by the component's source data.
test("latency artwork retargets finite transforms and settles without widening flat data", async ({
  page,
}) => {
  const profile = await completedProfile(page);
  await expect(profile.locator(".current-marker")).toHaveCount(0);
  const flat = await profile.evaluate((node) => ({
    rangeWidth: node.querySelector(".range")!.getBoundingClientRect().width,
    capWidths: [...node.querySelectorAll(".range-cap")].map(
      (cap) => cap.getBoundingClientRect().width,
    ),
  }));
  expect(flat.rangeWidth).toBe(0);
  expect(flat.capWidths).toEqual([1, 1]);
  const track = profile.locator(".track").first();
  const label = await track.getAttribute("aria-label");
  const movement = await track.evaluate(async (node) => {
    const mark = (node as HTMLElement).querySelector<HTMLElement>(
      ".position:has(.center-marker)",
    )!;
    const width = mark.getBoundingClientRect().width;
    const x = () => new DOMMatrix(getComputedStyle(mark).transform).m41 / width;
    mark.style.transform = "translateX(20%)";
    const first = mark.getAnimations()[0];
    first.pause();
    first.currentTime = 70;
    const moving = x();
    const keyframeProperties = mark
      .getAnimations()
      .flatMap((animation) =>
        (animation.effect as KeyframeEffect)
          .getKeyframes()
          .flatMap((frame) =>
            Object.keys(frame).filter(
              (key) =>
                !["offset", "computedOffset", "easing", "composite"].includes(
                  key,
                ),
            ),
          ),
      );
    mark.style.transform = "translateX(80%)";
    const retargeted = x();
    // Several arrivals inside one transition must preserve the visible position.
    for (const target of [30, 65, 40, 75]) {
      const transition = mark.getAnimations()[0];
      transition.pause();
      transition.currentTime = 35;
      mark.style.transform = `translateX(${target}%)`;
    }
    await new Promise((resolve) => setTimeout(resolve, 300));
    return {
      moving,
      retargeted,
      keyframeProperties,
      final: x(),
      running: mark.getAnimations().length,
    };
  });
  expect(movement.moving).toBeGreaterThan(0.2);
  expect(movement.moving).toBeLessThan(0.5);
  expect(Math.abs(movement.moving - movement.retargeted)).toBeLessThan(0.01);
  expect(new Set(movement.keyframeProperties)).toEqual(new Set(["transform"]));
  expect(Math.abs(movement.final - 0.75)).toBeLessThan(0.001);
  expect(movement.running).toBe(0);
  await expect(track).toHaveAttribute("aria-label", label!);
  await page.setViewportSize({ width: 320, height: 740 });
  expect(
    await profile.evaluate((node) => node.scrollWidth <= node.clientWidth),
  ).toBe(true);
  await track.focus();
  await expect(profile.locator(".hover-card")).toContainText("16.0");
  await expect(profile.locator(".guide")).toBeVisible();
  await expect(profile.locator(".pin")).toHaveCount(0);
});

test("latency motion snaps for reduced motion and hidden profiles", async ({
  page,
}) => {
  const profile = await completedProfile(page);
  const mark = profile.locator(".position:has(.center-marker)");
  await mark.evaluate((node) => {
    node.style.transform = "translateX(80%)";
  });
  await page.emulateMedia({ reducedMotion: "reduce" });
  await expect
    .poll(() => mark.evaluate((node) => node.getAnimations().length))
    .toBe(0);
  await page.emulateMedia({ reducedMotion: "no-preference" });
  await profile.evaluate((node) => {
    node.style.display = "none";
  });
  await expect(profile).toHaveAttribute("data-motion", "false");
  await mark.evaluate((node) => {
    node.style.transform = "translateX(20%)";
  });
  await expect
    .poll(() => mark.evaluate((node) => node.getAnimations().length))
    .toBe(0);
  await profile.evaluate((node) => {
    node.style.removeProperty("display");
  });
  await expect(profile).toHaveAttribute("data-motion", "true");
  await expect
    .poll(() => mark.evaluate((node) => node.getAnimations().length))
    .toBe(0);
  await page.evaluate(() => {
    Object.defineProperty(document, "hidden", {
      configurable: true,
      value: true,
    });
    document.dispatchEvent(new Event("visibilitychange"));
  });
  await expect(profile).toHaveAttribute("data-motion", "false");
  await mark.evaluate((node) => {
    node.style.transform = "translateX(70%)";
  });
  await expect
    .poll(() => mark.evaluate((node) => node.getAnimations().length))
    .toBe(0);
});
