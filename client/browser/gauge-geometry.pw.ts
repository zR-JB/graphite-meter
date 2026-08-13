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
