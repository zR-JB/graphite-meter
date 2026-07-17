import AxeBuilder from "@axe-core/playwright";
import { expect, test } from "@playwright/test";

test("settings stay editable during a run and expose accessible connection state", async ({
  page,
}) => {
  await page.goto("/?engine=dummy");
  await page.getByRole("button", { name: "Open settings" }).click();
  const settings = page.locator('[aria-label="Settings"]');

  await expect(
    settings.getByText("Ready", { exact: true }).first(),
  ).toBeVisible();
  await expect(
    settings.getByText("Fetch streams over HTTP/1.1 · clear"),
  ).toBeVisible();
  await expect(
    settings.getByLabel("Maximum H1 streams per direction"),
  ).toBeVisible();

  const advanced = settings.locator("summary", {
    hasText: "Customize the compensation model",
  });
  await advanced.focus();
  await advanced.press("Enter");
  await expect(settings.getByLabel("Connection profile")).toBeVisible();

  const accessibility = await new AxeBuilder({ page })
    .include('[aria-label="Settings"]')
    .analyze();
  expect(accessibility.violations).toEqual([]);

  await page.getByRole("button", { name: "Start the speed test" }).click();
  await expect(page.getByRole("button", { name: "Abort test" })).toBeVisible();
  await expect(settings.getByRole("button", { name: "short" })).toBeEnabled();
  await expect(settings.getByLabel("Unloaded ping cadence")).toBeEnabled();
  await expect(
    settings.getByLabel("Maximum H1 streams per direction"),
  ).toBeEnabled();
  await expect(
    settings.locator('input[name="throughput-target"]:checked'),
  ).toBeEnabled();
});

test("endpoint summary and diagnostics use accessible disclosure", async ({
  page,
}) => {
  await page.goto("/?engine=dummy");
  await page.getByRole("button", { name: "Toggle endpoint info" }).click();
  const endpoint = page.locator('[aria-label="Endpoint info"]');

  await expect(
    endpoint.getByText("Fetch stream · HTTP/1.1 · clear"),
  ).toBeVisible();
  await expect(
    endpoint.getByText("WebTransport streams · Fetch streams"),
  ).toBeVisible();
  await expect(
    endpoint.getByText("WebTransport datagrams · WebSocket"),
  ).toBeVisible();
  await expect(
    endpoint.getByText("Fetch streams over HTTP/1.1 · clear"),
  ).toBeVisible();
  const summary = endpoint.locator("summary", { hasText: "Diagnostics" });
  await summary.focus();
  await summary.press("Enter");
  await expect(
    endpoint.getByRole("button", { name: "Copy diagnostic report" }),
  ).toBeVisible();

  const accessibility = await new AxeBuilder({ page })
    .include('[aria-label="Endpoint info"]')
    .analyze();
  expect(accessibility.violations).toEqual([]);
});
