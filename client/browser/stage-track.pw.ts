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

test("terminal stage switches select the next run without erasing retained status", async ({
  page,
}) => {
  await page.goto("/?engine=dummy");
  await configureShortDownload(page);
  await page.getByRole("button", { name: "Start the speed test" }).click();
  await expect(
    page.getByRole("button", { name: "Run the test again" }),
  ).toBeVisible({ timeout: 5_000 });

  const download = page.getByRole("switch", { name: "Download stage" });
  await expect(download).toHaveAttribute("aria-checked", "true");
  await expect(download).toHaveClass(/seg--complete/);

  await download.click();
  await expect(download).toHaveAttribute("aria-checked", "false");
  await expect(download).toHaveClass(/seg--disabled/);
  await expect(download).toContainText("skipped");

  await download.click();
  await expect(download).toHaveAttribute("aria-checked", "true");
  await expect(download).toHaveClass(/seg--complete/);
});

test("stage switches preserve the at-least-one measured-stage guard", async ({
  page,
}) => {
  await page.goto("/?engine=dummy");
  const latency = page.getByRole("switch", { name: "Latency stage" });
  const download = page.getByRole("switch", { name: "Download stage" });
  const upload = page.getByRole("switch", { name: "Upload stage" });

  await latency.click();
  await download.click();
  await expect(upload).toHaveAttribute("aria-checked", "true");
  await upload.click();
  await expect(upload).toHaveAttribute("aria-checked", "true");
});

test("future-stage selection changes immediately during an active run", async ({
  page,
}) => {
  await page.goto("/?engine=dummy");
  await page.getByRole("button", { name: "Open settings" }).click();
  const settings = page.locator('[aria-label="Settings"]');
  await settings.getByRole("button", { name: "custom" }).click();
  for (const [label, value] of [
    ["Warmup ms", "0"],
    ["Latency ms", "1800"],
    ["Download ms", "900"],
    ["Upload ms", "0"],
  ] as const)
    await settings.getByLabel(label).fill(value);

  await page.getByRole("button", { name: "Start the speed test" }).click();
  const download = page.getByRole("switch", { name: "Download stage" });
  await expect(download).toBeEnabled();
  await download.click();
  await expect(download).toHaveAttribute("aria-checked", "false");
  await expect(download).toHaveClass(/seg--disabled/);
  await download.click();
  await expect(download).toHaveAttribute("aria-checked", "true");
  await expect(download).toHaveClass(/seg--pending/);

  await page.getByRole("button", { name: "Abort test" }).click();
  await expect(
    page.getByRole("button", { name: "Run the test again" }),
  ).toBeVisible();
  await expect(download).toHaveAttribute("aria-checked", "true");
});
