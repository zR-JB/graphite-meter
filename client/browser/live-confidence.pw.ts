import { expect, test } from "@playwright/test";

test("active bidirectional results present the emitted confidence", async ({
  page,
}) => {
  await page.goto("/?engine=dummy");
  await page.getByRole("button", { name: "Open settings" }).click();
  const settings = page.locator('[aria-label="Settings"]');
  await expect(
    settings.getByText("Ready", { exact: true }).first(),
  ).toBeVisible();
  await settings.getByRole("button", { name: "custom" }).click();
  await settings.getByText("Include concurrent download + upload").click();
  for (const [label, value] of [
    ["Warmup ms", "0"],
    ["Latency ms", "0"],
    ["Download ms", "0"],
    ["Upload ms", "0"],
    ["Bidirectional ms", "8000"],
  ] as const)
    await settings.getByLabel(label).fill(value);

  await page.getByRole("button", { name: "Start the speed test" }).click();
  const bidirectional = page.locator(".result-chip", { hasText: "Bi-dir" });
  await expect(bidirectional).toBeVisible();
  await expect(bidirectional.locator(".pip")).toHaveText("high", {
    timeout: 8_000,
  });
});
