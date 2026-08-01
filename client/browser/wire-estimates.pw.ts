import { expect, test, type Page } from "@playwright/test";

async function configureShortDownload(page: Page): Promise<void> {
  await page.getByRole("button", { name: "Open settings" }).click();
  const settings = page.locator('[aria-label="Settings"]');
  await expect(
    settings.getByText("Ready", { exact: true }).first(),
  ).toBeVisible();
  await settings.getByRole("button", { name: "custom" }).click();
  for (const [label, value] of [
    ["Warmup ms", "0"],
    ["Latency ms", "0"],
    ["Download ms", "900"],
    ["Upload ms", "0"],
  ] as const)
    await settings.getByLabel(label).fill(value);
}

test("default wire estimates stay secondary across live and completed views", async ({
  page,
}) => {
  await page.goto("/?engine=dummy");
  await configureShortDownload(page);
  await expect(page.getByLabel("Show estimated wire rate")).toBeChecked();

  await page.getByRole("button", { name: "Start the speed test" }).click();
  await expect(page.locator(".gauge-wire")).toContainText("wire estimate");
  await expect(page.locator(".result-chip .chip-wire")).toContainText(
    "wire estimate",
  );
  await expect(
    page.getByRole("button", { name: "Run the test again" }),
  ).toBeVisible({ timeout: 5_000 });
  await expect(page.locator(".gauge-wire")).toContainText("wire estimate");
  await expect(page.locator(".result-card .est")).toContainText(
    "wire estimate",
  );
});

test("a persisted opt-out hides only wire-estimate presentation", async ({
  page,
}) => {
  await page.goto("/?engine=dummy");
  await configureShortDownload(page);
  await page.getByText("Show estimated wire rate", { exact: true }).click();
  await expect(page.getByLabel("Show estimated wire rate")).not.toBeChecked();
  await expect
    .poll(() =>
      page.evaluate(() => {
        const raw = localStorage.getItem("graphite-meter:v1");
        return raw ? JSON.parse(raw).showWireEstimates : null;
      }),
    )
    .toBe(false);
  await page.reload();
  await page.getByRole("button", { name: "Open settings" }).click();
  await expect(page.getByLabel("Show estimated wire rate")).not.toBeChecked();

  await page.getByRole("button", { name: "Start the speed test" }).click();
  await expect(page.locator(".gauge-value")).not.toHaveText("—");
  await expect(
    page.locator(".gauge-wire, .chip-wire, .result-card .est"),
  ).toHaveCount(0);
});

test("loopback reports that no physical-wire estimate exists", async ({
  page,
}) => {
  await page.goto("/?engine=dummy");
  await configureShortDownload(page);
  const settings = page.locator('[aria-label="Settings"]');
  const advanced = settings.locator("summary", {
    hasText: "Customize the compensation model",
  });
  await advanced.click();
  await settings.getByLabel("Connection profile").selectOption("loopback");

  await page.getByRole("button", { name: "Start the speed test" }).click();
  await expect(page.locator(".gauge-wire, .chip-wire")).toHaveCount(0);
  await expect(
    page.getByRole("button", { name: "Run the test again" }),
  ).toBeVisible({ timeout: 5_000 });
  await expect(page.locator(".result-card .est")).toContainText(
    "no physical-wire estimate",
  );
});
