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

  const terminalCards = await page.evaluate(() => {
    const rail = document.querySelector(".stage-head");
    const cards = document.querySelector(".result-cards");
    if (!(rail instanceof HTMLElement) || !(cards instanceof HTMLElement))
      throw new Error("missing terminal result layout");
    return {
      railBottom: rail.getBoundingClientRect().bottom,
      cardsTop: cards.getBoundingClientRect().top,
      panelCenter:
        (document.querySelector(".gauge-panel")?.getBoundingClientRect().left ??
          0) +
        (document.querySelector(".gauge-panel")?.getBoundingClientRect()
          .width ?? 0) /
          2,
      cardsCenter:
        cards.getBoundingClientRect().left +
        cards.getBoundingClientRect().width / 2,
    };
  });
  expect(terminalCards.cardsTop).toBeGreaterThanOrEqual(
    terminalCards.railBottom - 1,
  );
  expect(
    Math.abs(terminalCards.cardsCenter - terminalCards.panelCenter),
  ).toBeLessThanOrEqual(1);

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

test("bidirectional stays a Settings-only optional stage", async ({ page }) => {
  await page.goto("/?engine=dummy");
  const bidi = page.getByRole("switch", { name: /Bidirectional stage/ });
  await expect(bidi).toHaveCount(0);

  await page.getByRole("button", { name: "Open settings" }).click();
  const settings = page.locator('[aria-label="Settings"]');
  const include = settings.getByLabel("Include concurrent download + upload");
  await include.check();
  await expect(bidi).toBeVisible();

  await bidi.click();
  await expect(bidi).toHaveCount(0);
  await expect(include).not.toBeChecked();
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

test("three live result chips remain below the phase rail", async ({
  page,
}) => {
  await page.goto("/?engine=dummy");
  await page.getByRole("button", { name: "Open settings" }).click();
  const settings = page.locator('[aria-label="Settings"]');
  await settings.getByRole("button", { name: "custom" }).click();
  for (const [label, value] of [
    ["Warmup ms", "0"],
    ["Latency ms", "250"],
    ["Download ms", "250"],
    ["Upload ms", "1400"],
  ] as const)
    await settings.getByLabel(label).fill(value);

  await page.getByRole("button", { name: "Start the speed test" }).click();
  await expect(page.locator(".result-chip")).toHaveCount(3, {
    timeout: 5_000,
  });
  const liveChips = await page.evaluate(() => {
    const rail = document.querySelector(".stage-head");
    const chips = document.querySelector(".result-chips");
    if (!(rail instanceof HTMLElement) || !(chips instanceof HTMLElement))
      throw new Error("missing live result layout");
    return {
      railBottom: rail.getBoundingClientRect().bottom,
      chipsTop: chips.getBoundingClientRect().top,
      panelCenter:
        (document.querySelector(".gauge-panel")?.getBoundingClientRect().left ??
          0) +
        (document.querySelector(".gauge-panel")?.getBoundingClientRect()
          .width ?? 0) /
          2,
      chipsCenter:
        chips.getBoundingClientRect().left +
        chips.getBoundingClientRect().width / 2,
    };
  });
  expect(liveChips.chipsTop).toBeGreaterThanOrEqual(liveChips.railBottom - 1);
  expect(
    Math.abs(liveChips.chipsCenter - liveChips.panelCenter),
  ).toBeLessThanOrEqual(1);
});
