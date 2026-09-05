import { expect, prepareApp, startAndWait, test } from "./webview";

test("completed bidirectional chart labels survive responsive layout updates", async ({
  page,
}) => {
  await page.emulateMedia({ reducedMotion: "reduce" });
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
  for (const [width, height] of [
    [1440, 900],
    [390, 844],
    [320, 740],
  ] as const) {
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
