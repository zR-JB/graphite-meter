import { expect, test } from "@playwright/test";

function persistedConfig(latency: boolean) {
  return JSON.stringify({
    config: {
      stages: {
        latency,
        download: true,
        upload: true,
        bidirectional: false,
      },
    },
  });
}

async function gaugeBox(
  page: import("@playwright/test").Page,
  latency: boolean,
) {
  await page.evaluate(
    (value) => localStorage.setItem("graphite-meter:v1", value),
    persistedConfig(latency),
  );
  await page.reload();
  const stage = page.locator(".gauge-panel .stage");
  await expect(stage).toBeVisible();
  return stage.evaluate((element) => {
    const box = element.getBoundingClientRect();
    const canvas = element.querySelector("canvas");
    return {
      width: box.width,
      height: box.height,
      canvasWidth: canvas?.getBoundingClientRect().width ?? 0,
      canvasHeight: canvas?.getBoundingClientRect().height ?? 0,
    };
  });
}

for (const viewport of [
  { width: 390, height: 640 },
  { width: 390, height: 844 },
]) {
  test(`mobile gauge geometry is invariant with latency at ${viewport.height}px`, async ({
    page,
  }) => {
    await page.setViewportSize(viewport);
    await page.goto("/?engine=dummy");
    const withLatency = await gaugeBox(page, true);
    const withoutLatency = await gaugeBox(page, false);

    expect(
      Math.abs(withLatency.width - withoutLatency.width),
    ).toBeLessThanOrEqual(1);
    expect(
      Math.abs(withLatency.height - withoutLatency.height),
    ).toBeLessThanOrEqual(1);
    expect(
      Math.abs(withLatency.canvasWidth - withoutLatency.canvasWidth),
    ).toBeLessThanOrEqual(1);
    expect(
      Math.abs(withLatency.canvasHeight - withoutLatency.canvasHeight),
    ).toBeLessThanOrEqual(1);
  });
}

test("gauge tick labels use shared optical anchors", async ({ page }) => {
  await page.goto("/?engine=dummy");
  await page.getByRole("button", { name: "Open settings" }).click();
  const settings = page.locator('[aria-label="Settings"]');
  await settings.getByRole("button", { name: "custom" }).click();
  for (const [label, value] of [
    ["Warmup ms", "0"],
    ["Latency ms", "1600"],
    ["Download ms", "0"],
    ["Upload ms", "0"],
  ] as const)
    await settings.getByLabel(label).fill(value);

  await page.getByRole("button", { name: "Start the speed test" }).click();
  await expect(page.locator("#console")).toHaveAttribute(
    "data-phase",
    "latency",
  );
  const anchors = await page.locator(".gauge-tick").evaluateAll((ticks) =>
    ticks.map((tick) => ({
      x: tick.getAttribute("data-anchor-x"),
      y: tick.getAttribute("data-anchor-y"),
    })),
  );
  expect(anchors).toEqual([
    { x: "start", y: "start" },
    { x: "start", y: "end" },
    { x: "center", y: "end" },
    { x: "end", y: "end" },
    { x: "end", y: "start" },
  ]);
});

for (const viewport of [
  { width: 1024, height: 640 },
  { width: 1024, height: 768 },
  { width: 1200, height: 800 },
  { width: 1366, height: 768 },
  { width: 1440, height: 900 },
]) {
  test(`desktop gauge fits the stage at ${viewport.width}x${viewport.height}`, async ({
    page,
  }) => {
    await page.setViewportSize(viewport);
    await page.goto("/?engine=dummy");
    const withLatency = await gaugeBox(page, true);
    const withoutLatency = await gaugeBox(page, false);
    expect(
      Math.abs(withLatency.width - withoutLatency.width),
    ).toBeLessThanOrEqual(1);
    expect(
      Math.abs(withLatency.height - withoutLatency.height),
    ).toBeLessThanOrEqual(1);
    const stage = await page
      .locator("#console > section.stage")
      .evaluate((element) => ({
        clientHeight: element.clientHeight,
        scrollHeight: element.scrollHeight,
      }));
    expect(stage.scrollHeight).toBeLessThanOrEqual(stage.clientHeight + 1);
  });
}
