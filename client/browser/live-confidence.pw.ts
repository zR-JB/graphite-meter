import { expect, test } from "@playwright/test";

test("live results stay measurement-first and defer confidence verdicts", async ({
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
  await expect(page.locator(".gauge-value")).toHaveAttribute(
    "aria-hidden",
    "true",
  );
  const bidirectional = page.locator(".result-chip", { hasText: "Bi-dir" });
  await expect(bidirectional).toBeVisible();
  await expect(bidirectional.locator(".chip-val")).toHaveAttribute(
    "aria-hidden",
    "true",
  );
  await expect(bidirectional.locator(".sr-only")).toContainText("Bi-dir:");
  await expect(bidirectional.locator(".pip")).toHaveCount(0);
  await expect(bidirectional).not.toContainText(/wire/i);
  await expect(bidirectional.locator(".chip-val .num")).not.toHaveText("—");

  await expect(
    page.getByRole("button", { name: "Run the test again" }),
  ).toBeVisible({ timeout: 10_000 });
  const completed = page.locator(".result-card", { hasText: "Bi-dir" });
  await expect(completed.locator(".pip")).toHaveText("high");
});
